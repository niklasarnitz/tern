//! Provider Drafts mailbox synchronization.

use super::{connect_and_login, validate_account, ImapError, Result, OPERATION_TIMEOUT};
use async_imap::{
    imap_proto::{types::AttributeValue, MailboxDatum, Response, Status},
    Session,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use mail_model::{Account, Draft, DraftMailboxSnapshot, RemoteDraft};
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::timeout,
};

const MAX_DRAFTS: usize = 500;
const MAX_DRAFT_BYTES: usize = 25 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
struct WireMetadata {
    id: String,
    revision: u64,
    recipients: Vec<String>,
    cc: Vec<String>,
    bcc: Vec<String>,
    subject: String,
}

/// Fetch all Tern-managed drafts from the provider's special-use Drafts mailbox.
/// Other clients' drafts are left untouched.
///
/// # Errors
/// Returns a sanitized error if discovery, authentication, or the bounded fetch fails.
pub async fn fetch_drafts(account: &Account, password: &str) -> Result<DraftMailboxSnapshot> {
    validate_account(account, password)?;
    let mut session = connect_and_login(account, password).await?;
    let remote_name = timeout(OPERATION_TIMEOUT, discover_drafts_mailbox(&mut session))
        .await
        .map_err(|_| ImapError::Timeout)??;
    let snapshot = timeout(
        OPERATION_TIMEOUT,
        fetch_drafts_session(&mut session, &remote_name),
    )
    .await
    .map_err(|_| ImapError::Timeout)??;
    let _ = timeout(OPERATION_TIMEOUT, session.logout()).await;
    Ok(snapshot)
}

/// Append a new revision, verify it by stable Tern id and content, then remove only
/// the prior UID. Updates require UIDPLUS so unrelated deleted mail is never expunged.
///
/// # Errors
/// Returns a sanitized error if the revision cannot be safely stored and verified.
pub async fn upload_draft(
    account: &Account,
    password: &str,
    remote_name: &str,
    draft: &Draft,
    revision: u64,
    old_uid_validity: Option<u32>,
    old_uid: Option<u32>,
) -> Result<RemoteDraft> {
    validate_account(account, password)?;
    let bytes = serialize_draft(account, draft, revision)?;
    let mut session = connect_and_login(account, password).await?;
    let capabilities = timeout(OPERATION_TIMEOUT, session.capabilities())
        .await
        .map_err(|_| ImapError::Timeout)?
        .map_err(|_| ImapError::Protocol)?;
    if old_uid.is_some() && !capabilities.has_str("UIDPLUS") {
        return Err(ImapError::DraftUpdateUnsupported);
    }
    timeout(
        OPERATION_TIMEOUT,
        session.append(remote_name, Some("\\Draft"), None, &bytes),
    )
    .await
    .map_err(|_| ImapError::Timeout)?
    .map_err(|_| ImapError::Protocol)?;

    let snapshot = timeout(
        OPERATION_TIMEOUT,
        fetch_drafts_session(&mut session, remote_name),
    )
    .await
    .map_err(|_| ImapError::Timeout)??;
    let uploaded = snapshot
        .drafts
        .iter()
        .filter(|remote| {
            remote.draft_id == draft.id
                && remote.revision == revision
                && content_matches(draft, remote)
        })
        .max_by_key(|remote| remote.uid)
        .cloned()
        .ok_or(ImapError::Protocol)?;

    if let Some(uid) = old_uid.filter(|uid| *uid != uploaded.uid) {
        if old_uid_validity != Some(snapshot.uid_validity) {
            return Err(ImapError::StaleMailbox);
        }
        timeout(
            OPERATION_TIMEOUT,
            delete_uid(&mut session, remote_name, snapshot.uid_validity, uid),
        )
        .await
        .map_err(|_| ImapError::Timeout)??;
    }
    let _ = timeout(OPERATION_TIMEOUT, session.logout()).await;
    Ok(uploaded)
}

/// Delete only the supplied stale draft UIDs using UIDPLUS.
///
/// # Errors
/// Returns an error instead of issuing broad EXPUNGE when safe targeted deletion
/// is unsupported or the mailbox generation changed.
pub async fn delete_draft_uids(
    account: &Account,
    password: &str,
    remote_name: &str,
    uid_validity: u32,
    uids: &[u32],
) -> Result<()> {
    if uids.is_empty() {
        return Ok(());
    }
    validate_account(account, password)?;
    let mut session = connect_and_login(account, password).await?;
    let capabilities = timeout(OPERATION_TIMEOUT, session.capabilities())
        .await
        .map_err(|_| ImapError::Timeout)?
        .map_err(|_| ImapError::Protocol)?;
    if !capabilities.has_str("UIDPLUS") {
        return Err(ImapError::DraftUpdateUnsupported);
    }
    let uid_set = uids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let mailbox = timeout(OPERATION_TIMEOUT, session.select(remote_name))
        .await
        .map_err(|_| ImapError::Timeout)?
        .map_err(|_| ImapError::Protocol)?;
    if mailbox.uid_validity != Some(uid_validity) {
        return Err(ImapError::StaleMailbox);
    }
    timeout(
        OPERATION_TIMEOUT,
        checked_command(
            &mut session,
            format!("UID STORE {uid_set} +FLAGS.SILENT (\\Deleted)"),
        ),
    )
    .await
    .map_err(|_| ImapError::Timeout)??;
    timeout(
        OPERATION_TIMEOUT,
        checked_command(&mut session, format!("UID EXPUNGE {uid_set}")),
    )
    .await
    .map_err(|_| ImapError::Timeout)??;
    let _ = timeout(OPERATION_TIMEOUT, session.logout()).await;
    Ok(())
}

async fn discover_drafts_mailbox<T>(session: &mut Session<T>) -> Result<String>
where
    T: AsyncRead + AsyncWrite + Unpin + Debug + Send,
{
    let tag = session
        .run_command("LIST \"\" \"*\"")
        .await
        .map_err(|_| ImapError::Protocol)?;
    let mut preferred = None;
    let mut fallback = None;
    loop {
        let response = session
            .read_response()
            .await
            .map_err(|_| ImapError::Protocol)?
            .ok_or(ImapError::Connection)?;
        match response.parsed() {
            Response::MailboxData(MailboxDatum::List {
                name_attributes,
                name,
                ..
            }) => {
                let name = name.to_string();
                if name_attributes
                    .iter()
                    .any(|attribute| matches!(attribute, imap_proto::types::NameAttribute::Drafts))
                {
                    preferred = Some(name);
                } else if name
                    .rsplit(['/', '.'])
                    .next()
                    .is_some_and(|part| part.eq_ignore_ascii_case("drafts"))
                {
                    fallback = Some(name);
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
                return preferred
                    .or(fallback)
                    .ok_or(ImapError::DraftsMailboxMissing);
            }
            _ => {}
        }
    }
}

async fn fetch_drafts_session<T>(
    session: &mut Session<T>,
    remote_name: &str,
) -> Result<DraftMailboxSnapshot>
where
    T: AsyncRead + AsyncWrite + Unpin + Debug + Send,
{
    let mailbox = session
        .examine(remote_name)
        .await
        .map_err(|_| ImapError::Protocol)?;
    let uid_validity = mailbox.uid_validity.ok_or(ImapError::MissingUidMetadata)?;
    if mailbox.exists == 0 {
        return Ok(DraftMailboxSnapshot {
            remote_name: remote_name.to_owned(),
            uid_validity,
            drafts: Vec::new(),
        });
    }
    let tag = session
        .run_command("UID FETCH 1:* (UID BODY.PEEK[])")
        .await
        .map_err(|_| ImapError::Protocol)?;
    let mut drafts = Vec::new();
    let mut total_bytes = 0_usize;
    loop {
        let response = session
            .read_response()
            .await
            .map_err(|_| ImapError::Protocol)?
            .ok_or(ImapError::Connection)?;
        match response.parsed() {
            Response::Fetch(_, attributes) => {
                let uid = attributes.iter().find_map(|attribute| match attribute {
                    AttributeValue::Uid(uid) => Some(*uid),
                    _ => None,
                });
                let raw = attributes.iter().find_map(|attribute| match attribute {
                    AttributeValue::BodySection {
                        data: Some(data), ..
                    } => Some(data.as_ref()),
                    _ => None,
                });
                if let (Some(uid), Some(raw)) = (uid, raw) {
                    total_bytes = total_bytes
                        .checked_add(raw.len())
                        .ok_or(ImapError::MessageTooLarge)?;
                    if total_bytes > MAX_DRAFT_BYTES || drafts.len() >= MAX_DRAFTS {
                        return Err(ImapError::MessageTooLarge);
                    }
                    if let Some(draft) = parse_draft(uid, raw) {
                        drafts.push(draft);
                    }
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
    Ok(DraftMailboxSnapshot {
        remote_name: remote_name.to_owned(),
        uid_validity,
        drafts,
    })
}

async fn delete_uid<T>(
    session: &mut Session<T>,
    remote_name: &str,
    expected_uid_validity: u32,
    uid: u32,
) -> Result<()>
where
    T: AsyncRead + AsyncWrite + Unpin + Debug + Send,
{
    let mailbox = session
        .select(remote_name)
        .await
        .map_err(|_| ImapError::Protocol)?;
    if mailbox.uid_validity != Some(expected_uid_validity) {
        return Err(ImapError::StaleMailbox);
    }
    checked_command(
        session,
        format!("UID STORE {uid} +FLAGS.SILENT (\\Deleted)"),
    )
    .await?;
    checked_command(session, format!("UID EXPUNGE {uid}")).await
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

fn serialize_draft(account: &Account, draft: &Draft, revision: u64) -> Result<Vec<u8>> {
    let valid_header = |value: &str| !value.contains(['\r', '\n']);
    if !valid_header(&account.email)
        || !valid_header(&draft.subject)
        || draft
            .recipients
            .iter()
            .chain(&draft.cc)
            .chain(&draft.bcc)
            .any(|address| !valid_header(address))
    {
        return Err(ImapError::InvalidConfiguration);
    }
    let metadata = WireMetadata {
        id: draft.id.clone(),
        revision,
        recipients: draft.recipients.clone(),
        cc: draft.cc.clone(),
        bcc: draft.bcc.clone(),
        subject: draft.subject.clone(),
    };
    let encoded = URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&metadata).map_err(|_| ImapError::InvalidConfiguration)?);
    let body = draft
        .body
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\n', "\r\n");
    let wire = format!(
        "From: {}\r\nTo: {}\r\nCc: {}\r\nBcc: {}\r\nSubject: {}\r\nX-Tern-Draft: {}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n{}",
        account.email,
        draft.recipients.join(", "),
        draft.cc.join(", "),
        draft.bcc.join(", "),
        draft.subject,
        encoded,
        body
    );
    if wire.len() > MAX_DRAFT_BYTES {
        return Err(ImapError::MessageTooLarge);
    }
    Ok(wire.into_bytes())
}

fn parse_draft(uid: u32, raw: &[u8]) -> Option<RemoteDraft> {
    let text = std::str::from_utf8(raw).ok()?;
    let (headers, body) = text
        .split_once("\r\n\r\n")
        .or_else(|| text.split_once("\n\n"))?;
    let encoded = headers
        .lines()
        .find_map(|line| line.strip_prefix("X-Tern-Draft:").map(str::trim))?;
    let metadata: WireMetadata =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(encoded).ok()?).ok()?;
    if metadata.id.is_empty() || metadata.revision == 0 {
        return None;
    }
    Some(RemoteDraft {
        draft_id: metadata.id,
        revision: metadata.revision,
        uid,
        recipients: metadata.recipients,
        cc: metadata.cc,
        bcc: metadata.bcc,
        subject: metadata.subject,
        body: body.replace("\r\n", "\n").replace('\r', "\n"),
    })
}

fn content_matches(draft: &Draft, remote: &RemoteDraft) -> bool {
    draft.recipients == remote.recipients
        && draft.cc == remote.cc
        && draft.bcc == remote.bcc
        && draft.subject == remote.subject
        && draft.body.replace("\r\n", "\n").replace('\r', "\n") == remote.body
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "Test fixtures fail immediately")]
mod tests {
    use super::*;
    use async_imap::Client;
    use mail_model::DraftSyncStatus;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    fn account() -> Account {
        Account {
            id: "a".into(),
            email: "author@example.test".into(),
            display_name: String::new(),
            imap_host: "imap.example.test".into(),
            imap_port: 993,
            username: "author".into(),
            credential_ref: "secret-ref".into(),
        }
    }

    fn draft() -> Draft {
        Draft {
            id: "draft-1".into(),
            account_id: "a".into(),
            recipients: vec!["One <one@example.test>".into()],
            cc: vec!["two@example.test".into()],
            bcc: vec!["hidden@example.test".into()],
            subject: "Grüße, team".into(),
            body: "first\nsecond".into(),
            updated_at: 1,
            sync_status: DraftSyncStatus::Pending,
        }
    }

    #[test]
    fn wire_round_trip_preserves_stable_identity_and_content() {
        let wire = serialize_draft(&account(), &draft(), 7).unwrap();
        let parsed = parse_draft(42, &wire).unwrap();
        assert_eq!(parsed.draft_id, "draft-1");
        assert_eq!(parsed.revision, 7);
        assert_eq!(parsed.uid, 42);
        assert!(content_matches(&draft(), &parsed));
    }

    #[test]
    fn ignores_non_tern_messages() {
        assert!(parse_draft(1, b"Subject: another client\r\n\r\nbody").is_none());
    }

    #[tokio::test]
    async fn discovers_special_use_mailbox_and_fetches_stable_draft() {
        let wire = serialize_draft(&account(), &draft(), 7).unwrap();
        let (client_stream, server_stream) = tokio::io::duplex(64 * 1024);
        let server = tokio::spawn(async move {
            let (reader, mut writer) = tokio::io::split(server_stream);
            let mut lines = BufReader::new(reader).lines();
            writer.write_all(b"* OK ready\r\n").await.unwrap();
            assert!(lines
                .next_line()
                .await
                .unwrap()
                .unwrap()
                .starts_with("A0001 LOGIN"));
            writer
                .write_all(b"A0001 OK authenticated\r\n")
                .await
                .unwrap();

            assert!(lines
                .next_line()
                .await
                .unwrap()
                .unwrap()
                .contains("LIST \"\" \"*\""));
            writer
                .write_all(
                    b"* LIST () \"/\" \"Drafts\"\r\n* LIST (\\Drafts) \"/\" \"Remote/Drafts\"\r\nA0002 OK listed\r\n",
                )
                .await
                .unwrap();

            assert!(lines
                .next_line()
                .await
                .unwrap()
                .unwrap()
                .contains("EXAMINE \"Remote/Drafts\""));
            writer
                .write_all(
                    b"* FLAGS (\\Draft)\r\n* 1 EXISTS\r\n* OK [UIDVALIDITY 11] valid\r\nA0003 OK examined\r\n",
                )
                .await
                .unwrap();
            assert!(lines
                .next_line()
                .await
                .unwrap()
                .unwrap()
                .contains("UID FETCH 1:* (UID BODY.PEEK[])"));
            let response = format!(
                "* 1 FETCH (UID 42 BODY[] {{{}}}\r\n{})\r\nA0004 OK fetched\r\n",
                wire.len(),
                String::from_utf8(wire).unwrap()
            );
            writer.write_all(response.as_bytes()).await.unwrap();
        });

        let mut client = Client::new(client_stream);
        client.read_response().await.unwrap();
        let mut session = client.login("test", "test").await.unwrap();
        let remote_name = discover_drafts_mailbox(&mut session).await.unwrap();
        assert_eq!(remote_name, "Remote/Drafts");
        let snapshot = fetch_drafts_session(&mut session, &remote_name)
            .await
            .unwrap();
        assert_eq!(snapshot.uid_validity, 11);
        assert_eq!(snapshot.drafts.len(), 1);
        assert_eq!(snapshot.drafts[0].draft_id, "draft-1");
        assert_eq!(snapshot.drafts[0].revision, 7);
        server.await.unwrap();
    }
}
