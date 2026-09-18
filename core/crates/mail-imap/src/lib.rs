//! Verified IMAP synchronization and queued application operations.
//!
//! Connections use implicit TLS on the configured port and the platform
//! certificate store.  This crate intentionally does not offer STARTTLS, certificate
//! overrides, or password authentication for Gmail; those choices keep the
//! initial network boundary small and safe while OAuth support is added.

use std::{convert::TryFrom, fmt::Debug, sync::Arc, time::Duration};

use async_imap::{
    extensions::idle::IdleResponse,
    imap_proto::{
        types::{AttributeValue, MessageSection, SectionPath},
        MailboxDatum, Response, Status,
    },
    Client, Session,
};
use chrono::{DateTime, FixedOffset};
use mail_model::{Account, MailboxSnapshot};
use rustls::{pki_types::ServerName, ClientConfig, RootCertStore};
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpStream,
    time::timeout,
};
use tokio_rustls::TlsConnector;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const OPERATION_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_IDLE_WAIT: Duration = Duration::from_mins(25);
const MAX_HEADERS: u32 = 100;

mod conversation;
pub use conversation::{apply_operation, fetch_mailbox};

type TlsStream = tokio_rustls::client::TlsStream<TcpStream>;
type ImapSession = Session<TlsStream>;

/// Errors returned by the IMAP boundary.  Diagnostics intentionally avoid
/// forwarding server strings, which can contain account addresses or command
/// details, and never include the supplied password.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ImapError {
    #[error("IMAP configuration is invalid")]
    InvalidConfiguration,
    #[error("Gmail requires OAuth authentication")]
    GmailRequiresOAuth,
    #[error("the IMAP operation timed out")]
    Timeout,
    #[error("the TLS connection could not be established")]
    Tls,
    #[error("the platform certificate store is unavailable")]
    CertificateStore,
    #[error("the IMAP connection failed")]
    Connection,
    #[error("IMAP authentication failed")]
    Authentication,
    #[error("the IMAP server returned an invalid response")]
    Protocol,
    #[error("the server did not provide mailbox UID metadata")]
    MissingUidMetadata,
    #[error("the server did not provide a message header")]
    MissingHeader,
    #[error("the message exceeds the supported download limit")]
    MessageTooLarge,
    #[error("the message body could not be decoded")]
    InvalidMime,
    #[error("the mailbox identity changed; this operation cannot be replayed")]
    StaleMailbox,
    #[error("the server does not support safe message moves")]
    MoveUnsupported,
}

impl ImapError {
    /// Whether reconnecting may succeed without changing account configuration.
    /// Every retry creates a new socket, re-resolves DNS, and therefore follows
    /// Wi-Fi/cellular and IPv4/IPv6 route changes.
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        matches!(self, Self::Timeout | Self::Connection)
    }
}

pub type Result<T> = std::result::Result<T, ImapError>;

/// Fetch the newest at most 100 headers from the user's INBOX.
///
/// # Errors
///
/// Returns a sanitized [`ImapError`] when the account configuration is
/// invalid, Gmail password authentication is requested, TLS verification or
/// the connection fails, the server authentication fails, or the server
/// returns an incomplete response.
pub async fn fetch_inbox(account: &Account, password: &str) -> Result<MailboxSnapshot> {
    validate_account(account, password)?;
    let mut session = connect_and_login(account, password).await?;

    let snapshot = timeout(OPERATION_TIMEOUT, fetch_inbox_session(&mut session))
        .await
        .map_err(|_| ImapError::Timeout)??;
    let _ = timeout(OPERATION_TIMEOUT, session.logout()).await;
    Ok(snapshot)
}

async fn fetch_inbox_session<T>(session: &mut Session<T>) -> Result<MailboxSnapshot>
where
    T: AsyncRead + AsyncWrite + Unpin + Debug + Send,
{
    fetch_headers_session(session, "INBOX", false).await
}

async fn fetch_headers_session<T>(
    session: &mut Session<T>,
    remote_name: &str,
    gmail: bool,
) -> Result<MailboxSnapshot>
where
    T: AsyncRead + AsyncWrite + Unpin + Debug + Send,
{
    // EXAMINE keeps header reads strictly read-only.  It does not mark
    // recent messages as seen and cannot mutate mailbox state.
    let mailbox = timeout(OPERATION_TIMEOUT, session.examine(remote_name))
        .await
        .map_err(|_| ImapError::Timeout)?
        .map_err(|error| classify_imap_error(error, ImapError::Protocol))?;
    let uid_validity = mailbox.uid_validity.ok_or(ImapError::MissingUidMetadata)?;

    let headers = if mailbox.exists == 0 {
        Vec::new()
    } else {
        let first_sequence = mailbox.exists.saturating_sub(MAX_HEADERS - 1).max(1);
        let sequence_set = format!("{first_sequence}:{}", mailbox.exists);
        let provider_fields = if gmail { "X-GM-MSGID X-GM-THRID " } else { "" };
        let request_id = timeout(
            OPERATION_TIMEOUT,
            session.run_command(format!(
                "FETCH {sequence_set} (UID FLAGS INTERNALDATE {provider_fields}BODY.PEEK[HEADER.FIELDS (MESSAGE-ID IN-REPLY-TO REFERENCES SUBJECT FROM TO CC BCC REPLY-TO DATE LIST-ID LIST-POST LIST-UNSUBSCRIBE AUTHENTICATION-RESULTS RECEIVED-SPF)])"
            )),
        )
        .await
        .map_err(|_| ImapError::Timeout)?
        .map_err(|error| classify_imap_error(error, ImapError::Protocol))?;

        let mut headers = Vec::with_capacity(MAX_HEADERS as usize);
        let mut problem = None;
        let mut overflow = false;
        loop {
            let response = timeout(OPERATION_TIMEOUT, session.read_response())
                .await
                .map_err(|_| ImapError::Timeout)?
                .map_err(|error| classify_io_error(&error))?
                .ok_or(ImapError::Connection)?;

            match response.parsed() {
                Response::Fetch(_, attributes) => {
                    if headers.len() >= MAX_HEADERS as usize {
                        overflow = true;
                        continue;
                    }
                    let fetch = parse_fetch_attributes(attributes);
                    let Some(uid) = fetch.uid else {
                        problem.get_or_insert(ImapError::MissingUidMetadata);
                        continue;
                    };
                    let Some(raw_header) = fetch.raw_header.filter(|header| !header.is_empty())
                    else {
                        problem.get_or_insert(ImapError::MissingHeader);
                        continue;
                    };
                    let mut header =
                        mail_mime::parse_header(uid, raw_header, fetch.is_read, fetch.is_starred);
                    // INTERNALDATE is the server arrival timestamp and is the
                    // canonical date for this synchronization snapshot.
                    if let Some(internal_date) = fetch.internal_date.and_then(parse_internal_date) {
                        header.date = internal_date.to_rfc3339();
                        header.sent_at = Some(internal_date.timestamp());
                    }
                    header.provider_message_id = attributes.iter().find_map(|attribute| {
                        if let AttributeValue::GmailMsgId(value) = attribute {
                            Some(value.to_string())
                        } else {
                            None
                        }
                    });
                    header.provider_thread_id = attributes.iter().find_map(|attribute| {
                        if let AttributeValue::GmailThrId(value) = attribute {
                            Some(value.to_string())
                        } else {
                            None
                        }
                    });
                    headers.push(header);
                }
                Response::Done { tag, status, .. } if tag == &request_id => {
                    if status != &Status::Ok {
                        return Err(ImapError::Protocol);
                    }
                    break;
                }
                _ => {}
            }
        }

        if overflow {
            return Err(ImapError::Protocol);
        }
        if let Some(problem) = problem {
            return Err(problem);
        }
        // UIDs generally increase with arrival.  Sorting makes the result
        // deterministic even when a server emits FETCH responses out of order.
        headers.sort_by_key(|header| std::cmp::Reverse(header.uid));
        headers
    };

    Ok(MailboxSnapshot {
        remote_name: remote_name.to_owned(),
        uid_validity,
        uid_next: mailbox.uid_next,
        headers,
    })
}

/// List selectable remote mailbox names using a read-only authenticated
/// session.  Names are returned exactly as supplied by the server.
///
/// # Errors
///
/// Returns a sanitized [`ImapError`] when the account configuration is
/// invalid, Gmail password authentication is requested, TLS verification or
/// the connection fails, the server authentication fails, or the server
/// returns an incomplete response.
pub async fn list_remote_mailboxes(account: &Account, password: &str) -> Result<Vec<String>> {
    validate_account(account, password)?;
    let mut session = connect_and_login(account, password).await?;
    let request_id = timeout(OPERATION_TIMEOUT, session.run_command("LIST \"\" \"*\""))
        .await
        .map_err(|_| ImapError::Timeout)?
        .map_err(|error| classify_imap_error(error, ImapError::Protocol))?;

    let mut names = Vec::new();
    loop {
        let response = timeout(OPERATION_TIMEOUT, session.read_response())
            .await
            .map_err(|_| ImapError::Timeout)?
            .map_err(|error| classify_io_error(&error))?
            .ok_or(ImapError::Connection)?;
        match response.parsed() {
            Response::MailboxData(MailboxDatum::List {
                name_attributes,
                name,
                ..
            }) if !name_attributes.iter().any(|attribute| {
                matches!(attribute, imap_proto::types::NameAttribute::NoSelect)
            }) =>
            {
                names.push(name.to_string());
            }
            Response::Done { tag, status, .. } if tag == &request_id => {
                if status != &Status::Ok {
                    return Err(ImapError::Protocol);
                }
                break;
            }
            _ => {}
        }
    }

    let _ = timeout(OPERATION_TIMEOUT, session.logout()).await;
    Ok(names)
}

/// Wait until the Inbox may have changed, or until a periodic refresh is due.
///
/// Servers advertising IDLE are monitored for at most 25 minutes before the
/// connection is renewed. Other servers use the same interval as a polling
/// delay. A dropped IDLE stream is reported as a retryable connection error so
/// orchestration can reconnect without holding a stale socket.
///
/// # Errors
/// Returns a sanitized error if configuration, connection, TLS,
/// authentication, mailbox selection, or IDLE negotiation fails.
pub async fn wait_for_inbox_change(
    account: &Account,
    password: &str,
    maximum_wait: Duration,
) -> Result<()> {
    validate_account(account, password)?;
    if maximum_wait.is_zero() {
        return Err(ImapError::InvalidConfiguration);
    }
    let session = connect_and_login(account, password).await?;
    wait_for_mailbox_change_session(session, "INBOX", maximum_wait.min(MAX_IDLE_WAIT)).await
}

async fn wait_for_mailbox_change_session<T>(
    mut session: Session<T>,
    remote_name: &str,
    maximum_wait: Duration,
) -> Result<()>
where
    T: AsyncRead + AsyncWrite + Unpin + Debug + Send,
{
    let capabilities = timeout(OPERATION_TIMEOUT, session.capabilities())
        .await
        .map_err(|_| ImapError::Timeout)?
        .map_err(|error| classify_imap_error(error, ImapError::Protocol))?;
    timeout(OPERATION_TIMEOUT, session.examine(remote_name))
        .await
        .map_err(|_| ImapError::Timeout)?
        .map_err(|error| classify_imap_error(error, ImapError::Protocol))?;

    if !capabilities.has_str("IDLE") {
        tokio::time::sleep(maximum_wait).await;
        let _ = timeout(OPERATION_TIMEOUT, session.logout()).await;
        return Ok(());
    }

    let mut idle = session.idle();
    timeout(OPERATION_TIMEOUT, idle.init())
        .await
        .map_err(|_| ImapError::Timeout)?
        .map_err(|error| classify_imap_error(error, ImapError::Protocol))?;
    let response = {
        let (waiting, _interrupt) = idle.wait_with_timeout(maximum_wait);
        waiting
            .await
            .map_err(|error| classify_imap_error(error, ImapError::Protocol))?
    };
    if response == IdleResponse::ManualInterrupt {
        return Err(ImapError::Connection);
    }

    let mut session = timeout(OPERATION_TIMEOUT, idle.done())
        .await
        .map_err(|_| ImapError::Timeout)?
        .map_err(|error| classify_imap_error(error, ImapError::Protocol))?;
    let _ = timeout(OPERATION_TIMEOUT, session.logout()).await;
    Ok(())
}

fn validate_account(account: &Account, password: &str) -> Result<()> {
    let host = account.imap_host.trim_end_matches('.');
    if host.is_empty()
        || host
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
        || account.imap_port == 0
        || account.username.is_empty()
        || password.is_empty()
    {
        return Err(ImapError::InvalidConfiguration);
    }

    if host.eq_ignore_ascii_case("imap.gmail.com")
        || host.eq_ignore_ascii_case("imap.googlemail.com")
    {
        return Err(ImapError::GmailRequiresOAuth);
    }

    Ok(())
}

async fn connect_and_login(account: &Account, password: &str) -> Result<ImapSession> {
    let config = tls_config()?;
    connect_and_login_with_config(account, password, config).await
}

async fn connect_and_login_with_config(
    account: &Account,
    password: &str,
    config: ClientConfig,
) -> Result<ImapSession> {
    let host = account.imap_host.trim_end_matches('.');
    let tcp = timeout(
        CONNECT_TIMEOUT,
        TcpStream::connect((host, account.imap_port)),
    )
    .await
    .map_err(|_| ImapError::Timeout)?
    .map_err(|_| ImapError::Connection)?;

    let connector = TlsConnector::from(Arc::new(config));
    let server_name =
        ServerName::try_from(host.to_owned()).map_err(|_| ImapError::InvalidConfiguration)?;
    let tls_stream = timeout(CONNECT_TIMEOUT, connector.connect(server_name, tcp))
        .await
        .map_err(|_| ImapError::Timeout)?
        .map_err(|error| classify_tls_error(&error))?;

    let mut client = Client::new(tls_stream);
    let greeting = timeout(OPERATION_TIMEOUT, client.read_response())
        .await
        .map_err(|_| ImapError::Timeout)?
        .map_err(|error| classify_io_error(&error))?;
    if greeting.is_none() {
        return Err(ImapError::Connection);
    }

    let session = timeout(OPERATION_TIMEOUT, client.login(&account.username, password))
        .await
        .map_err(|_| ImapError::Timeout)?
        .map_err(|(error, _client)| classify_imap_error(error, ImapError::Authentication))?;
    Ok(session)
}

fn classify_imap_error(error: async_imap::error::Error, fallback: ImapError) -> ImapError {
    match error {
        async_imap::error::Error::Io(error) => classify_io_error(&error),
        async_imap::error::Error::ConnectionLost => ImapError::Connection,
        _ => fallback,
    }
}

fn classify_io_error(error: &std::io::Error) -> ImapError {
    if error.kind() == std::io::ErrorKind::InvalidData {
        ImapError::Protocol
    } else {
        ImapError::Connection
    }
}

fn classify_tls_error(error: &std::io::Error) -> ImapError {
    if error.kind() == std::io::ErrorKind::InvalidData {
        ImapError::Tls
    } else {
        ImapError::Connection
    }
}

fn parse_internal_date(value: &str) -> Option<DateTime<FixedOffset>> {
    DateTime::parse_from_str(value, "%d-%b-%Y %H:%M:%S %z").ok()
}

struct FetchAttributes<'a> {
    uid: Option<u32>,
    is_read: bool,
    is_starred: bool,
    internal_date: Option<&'a str>,
    raw_header: Option<&'a [u8]>,
}

fn parse_fetch_attributes<'a>(attributes: &'a [AttributeValue<'a>]) -> FetchAttributes<'a> {
    let mut parsed = FetchAttributes {
        uid: None,
        is_read: false,
        is_starred: false,
        internal_date: None,
        raw_header: None,
    };
    for attribute in attributes {
        match attribute {
            AttributeValue::Uid(value) => parsed.uid = Some(*value),
            AttributeValue::Flags(flags) => {
                parsed.is_read |= flags.iter().any(|flag| flag.eq_ignore_ascii_case("\\Seen"));
                parsed.is_starred |= flags
                    .iter()
                    .any(|flag| flag.eq_ignore_ascii_case("\\Flagged"));
            }
            AttributeValue::InternalDate(value) => parsed.internal_date = Some(value.as_ref()),
            AttributeValue::BodySection {
                section: Some(SectionPath::Full(MessageSection::Header)),
                data: Some(value),
                ..
            }
            | AttributeValue::Rfc822Header(Some(value)) => parsed.raw_header = Some(value.as_ref()),
            _ => {}
        }
    }
    parsed
}

fn tls_config() -> Result<ClientConfig> {
    let native = rustls_native_certs::load_native_certs();
    if native.certs.is_empty() {
        return Err(ImapError::CertificateStore);
    }

    let mut roots = RootCertStore::empty();
    for certificate in native.certs {
        roots
            .add(certificate)
            .map_err(|_| ImapError::CertificateStore)?;
    }

    Ok(ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::{fmt::Write as _, sync::Arc, time::Duration};

    use super::{classify_tls_error, validate_account, ImapError};
    use async_imap::Client;
    use mail_model::Account;
    use rcgen::generate_simple_self_signed;
    use rustls::{
        pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
        ClientConfig, RootCertStore, ServerConfig,
    };
    use tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
        net::TcpListener,
        time::timeout,
    };
    use tokio_rustls::TlsAcceptor;

    fn account(host: &str) -> Account {
        Account {
            id: "account".into(),
            email: "user@example.test".into(),
            display_name: "User".into(),
            imap_host: host.into(),
            imap_port: 993,
            username: "user@example.test".into(),
            credential_ref: "keychain:item".into(),
        }
    }

    #[test]
    fn rejects_gmail_password_auth() {
        assert_eq!(
            validate_account(&account("IMAP.GMAIL.COM."), "secret"),
            Err(ImapError::GmailRequiresOAuth)
        );
    }

    #[test]
    fn requires_nonzero_implicit_tls_port() {
        let mut account = account("imap.example.test");
        account.imap_port = 0;
        assert_eq!(
            validate_account(&account, "secret"),
            Err(ImapError::InvalidConfiguration)
        );
    }

    #[test]
    fn builds_tls_config_with_platform_roots() {
        assert!(super::tls_config().is_ok());
    }

    #[test]
    fn retries_transport_tls_failures_but_not_invalid_certificates() {
        assert_eq!(
            classify_tls_error(&std::io::Error::from(std::io::ErrorKind::ConnectionReset,)),
            ImapError::Connection
        );
        assert_eq!(
            classify_tls_error(&std::io::Error::from(std::io::ErrorKind::InvalidData)),
            ImapError::Tls
        );
        assert!(ImapError::Connection.is_retryable());
        assert!(ImapError::Timeout.is_retryable());
        assert!(!ImapError::Tls.is_retryable());
        assert!(!ImapError::Authentication.is_retryable());
    }

    #[tokio::test]
    async fn dropped_idle_session_is_a_retryable_connection_failure() {
        let (client_stream, server_stream) = tokio::io::duplex(4096);
        let server = tokio::spawn(async move {
            let (reader, mut writer) = tokio::io::split(server_stream);
            let mut lines = BufReader::new(reader).lines();
            writer.write_all(b"* OK ready\r\n").await.unwrap();

            assert!(lines.next_line().await.unwrap().unwrap().contains("LOGIN"));
            writer.write_all(b"A0001 OK logged in\r\n").await.unwrap();
            assert!(lines
                .next_line()
                .await
                .unwrap()
                .unwrap()
                .contains("CAPABILITY"));
            writer
                .write_all(b"* CAPABILITY IMAP4rev1 IDLE\r\nA0002 OK capabilities\r\n")
                .await
                .unwrap();
            assert!(lines
                .next_line()
                .await
                .unwrap()
                .unwrap()
                .contains("EXAMINE"));
            writer
                .write_all(
                    b"* FLAGS (\\Seen)\r\n* 0 EXISTS\r\n* OK [UIDVALIDITY 99] valid\r\nA0003 OK [READ-ONLY] examined\r\n",
                )
                .await
                .unwrap();
            assert!(lines.next_line().await.unwrap().unwrap().contains("IDLE"));
            writer.write_all(b"+ idling\r\n").await.unwrap();
        });

        let mut client = Client::new(client_stream);
        assert!(client.read_response().await.unwrap().is_some());
        let session = client.login("user", "password").await.unwrap();
        let result =
            super::wait_for_mailbox_change_session(session, "INBOX", Duration::from_mins(1)).await;
        assert_eq!(result, Err(ImapError::Connection));
        assert!(result.unwrap_err().is_retryable());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn idle_notification_completes_the_command_before_returning() {
        let (client_stream, server_stream) = tokio::io::duplex(4096);
        let server = tokio::spawn(async move {
            let (reader, mut writer) = tokio::io::split(server_stream);
            let mut lines = BufReader::new(reader).lines();
            writer.write_all(b"* OK ready\r\n").await.unwrap();
            assert!(lines.next_line().await.unwrap().unwrap().contains("LOGIN"));
            writer.write_all(b"A0001 OK logged in\r\n").await.unwrap();
            assert!(lines
                .next_line()
                .await
                .unwrap()
                .unwrap()
                .contains("CAPABILITY"));
            writer
                .write_all(b"* CAPABILITY IMAP4rev1 IDLE\r\nA0002 OK capabilities\r\n")
                .await
                .unwrap();
            assert!(lines
                .next_line()
                .await
                .unwrap()
                .unwrap()
                .contains("EXAMINE"));
            writer
                .write_all(
                    b"* FLAGS (\\Seen)\r\n* 0 EXISTS\r\n* OK [UIDVALIDITY 99] valid\r\nA0003 OK [READ-ONLY] examined\r\n",
                )
                .await
                .unwrap();
            assert!(lines.next_line().await.unwrap().unwrap().contains("IDLE"));
            writer
                .write_all(b"+ idling\r\n* 1 EXISTS\r\n")
                .await
                .unwrap();
            assert_eq!(lines.next_line().await.unwrap().unwrap(), "DONE");
            writer.write_all(b"A0004 OK idle done\r\n").await.unwrap();
            assert!(lines.next_line().await.unwrap().unwrap().contains("LOGOUT"));
            writer
                .write_all(b"* BYE closing\r\nA0005 OK logout\r\n")
                .await
                .unwrap();
        });

        let mut client = Client::new(client_stream);
        assert!(client.read_response().await.unwrap().is_some());
        let session = client.login("user", "password").await.unwrap();
        super::wait_for_mailbox_change_session(session, "INBOX", Duration::from_mins(1))
            .await
            .unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn reads_header_fields_from_imap_fetch_response() {
        use tokio::io::AsyncWriteExt;

        let header = concat!(
            "Message-ID: <wire@example.test>\r\n",
            "From: Wire Sender <sender@example.test>\r\n",
            "Subject: Wire subject\r\n",
            "Date: Thu, 17 Sep 2026 07:30:00 +0000\r\n",
            "\r\n",
        );
        let response = format!(
            "* OK ready\r\n\
A0001 OK logged in\r\n\
* FLAGS (\\Seen \\Flagged)\r\n\
* 1 EXISTS\r\n\
* 0 RECENT\r\n\
* OK [UIDVALIDITY 99] valid\r\n\
* OK [UIDNEXT 10] next\r\n\
A0002 OK [READ-ONLY] examined\r\n\
* 1 FETCH (UID 9 FLAGS (\\Seen \\Flagged) INTERNALDATE \"17-Sep-2026 08:30:00 +0100\" BODY[HEADER.FIELDS (MESSAGE-ID SUBJECT FROM DATE)] {{{}}}\r\n{} )\r\n\
A0003 OK fetched\r\n",
            header.len(),
            header
        );
        let (client_stream, mut server_stream) = tokio::io::duplex(16 * 1024);
        tokio::spawn(async move {
            server_stream.write_all(response.as_bytes()).await.unwrap();
            tokio::time::sleep(Duration::from_secs(1)).await;
        });

        let mut client = Client::new(client_stream);
        assert!(client.read_response().await.unwrap().is_some());
        let mut session = client.login("user", "password").await.unwrap();
        let snapshot = super::fetch_inbox_session(&mut session).await.unwrap();

        assert_eq!(snapshot.remote_name, "INBOX");
        assert_eq!(snapshot.uid_validity, 99);
        assert_eq!(snapshot.uid_next, Some(10));
        assert_eq!(snapshot.headers.len(), 1);
        assert_eq!(snapshot.headers[0].uid, 9);
        assert_eq!(snapshot.headers[0].subject, "Wire subject");
        assert_eq!(
            snapshot.headers[0].sender,
            "Wire Sender <sender@example.test>"
        );
        assert!(snapshot.headers[0].is_read);
        assert!(snapshot.headers[0].is_starred);
        assert_eq!(snapshot.headers[0].date, "2026-09-17T08:30:00+01:00");
    }

    #[tokio::test]
    async fn empty_mailbox_does_not_issue_fetch() {
        let response = concat!(
            "* OK ready\r\n",
            "A0001 OK logged in\r\n",
            "* FLAGS (\\Seen)\r\n",
            "* 0 EXISTS\r\n",
            "* 0 RECENT\r\n",
            "* OK [UIDVALIDITY 99] valid\r\n",
            "* OK [UIDNEXT 1] next\r\n",
            "A0002 OK [READ-ONLY] examined\r\n",
        );
        let (client_stream, mut server_stream) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            server_stream.write_all(response.as_bytes()).await.unwrap();
            tokio::time::sleep(Duration::from_millis(250)).await;
        });

        let mut client = Client::new(client_stream);
        assert!(client.read_response().await.unwrap().is_some());
        let mut session = client.login("user", "password").await.unwrap();
        let snapshot = super::fetch_inbox_session(&mut session).await.unwrap();
        assert!(snapshot.headers.is_empty());
    }

    #[tokio::test]
    async fn drains_one_hundred_headers_and_rejects_tagged_no() {
        let header = concat!(
            "Message-ID: <wire@example.test>\r\n",
            "From: Wire Sender <sender@example.test>\r\n",
            "Subject: Wire subject\r\n",
            "\r\n",
        );
        let mut response = String::from(
            "* OK ready\r\n\
A0001 OK logged in\r\n\
* FLAGS (\\Seen)\r\n\
* 100 EXISTS\r\n\
* 0 RECENT\r\n\
* OK [UIDVALIDITY 99] valid\r\n\
* OK [UIDNEXT 101] next\r\n\
A0002 OK [READ-ONLY] examined\r\n",
        );
        for sequence in 1..=100 {
            let _ = writeln!(
                response,
                "* {sequence} FETCH (UID {sequence} FLAGS () INTERNALDATE \"17-Sep-2026 08:30:00 +0000\" BODY[HEADER.FIELDS (MESSAGE-ID SUBJECT FROM)] {{{}}}\r\n{})\r",
                header.len(),
                header
            );
        }
        response.push_str("A0003 NO fetch failed\r\n");

        let (client_stream, mut server_stream) = tokio::io::duplex(128 * 1024);
        tokio::spawn(async move {
            server_stream.write_all(response.as_bytes()).await.unwrap();
            tokio::time::sleep(Duration::from_millis(250)).await;
        });

        let mut client = Client::new(client_stream);
        assert!(client.read_response().await.unwrap().is_some());
        let mut session = client.login("user", "password").await.unwrap();
        let result = super::fetch_inbox_session(&mut session).await;
        assert!(matches!(result, Err(ImapError::Protocol)));
    }

    #[tokio::test]
    async fn verified_tls_script_checks_login_examine_and_fetch_commands() {
        let (client_config, server_config, certificate) = test_tls_configs();
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let acceptor = TlsAcceptor::from(Arc::new(server_config));
            let tls = acceptor.accept(tcp).await.unwrap();
            let (reader, mut writer) = tokio::io::split(tls);
            let mut lines = BufReader::new(reader).lines();
            writer.write_all(b"* OK ready\r\n").await.unwrap();

            let login = lines.next_line().await.unwrap().unwrap();
            assert!(login.starts_with("A0001 LOGIN \"user@example.test\" \"password\""));
            writer.write_all(b"A0001 OK logged in\r\n").await.unwrap();

            let examine = lines.next_line().await.unwrap().unwrap();
            assert_eq!(examine, "A0002 EXAMINE \"INBOX\"");
            writer
                .write_all(
                    b"* FLAGS (\\Seen)\r\n* 1 EXISTS\r\n* 0 RECENT\r\n* OK [UIDVALIDITY 99] valid\r\n* OK [UIDNEXT 2] next\r\nA0002 OK [READ-ONLY] examined\r\n",
                )
                .await
                .unwrap();

            let fetch = lines.next_line().await.unwrap().unwrap();
            assert!(fetch.starts_with("A0003 FETCH 1:1"));
            assert!(fetch.contains("BODY.PEEK[HEADER.FIELDS"));
            let header = "Message-ID: <tls@example.test>\r\nSubject: TLS\r\nFrom: TLS <tls@example.test>\r\n\r\n";
            let fetch_response = format!(
                "* 1 FETCH (UID 1 FLAGS (\\Seen) INTERNALDATE \"17-Sep-2026 08:30:00 +0000\" BODY[HEADER.FIELDS (MESSAGE-ID SUBJECT FROM DATE)] {{{}}}\r\n{})\r\nA0003 OK fetched\r\n",
                header.len(),
                header
            );
            writer.write_all(fetch_response.as_bytes()).await.unwrap();

            let logout = lines.next_line().await.unwrap().unwrap();
            assert_eq!(logout, "A0004 LOGOUT");
            writer
                .write_all(b"* BYE closing\r\nA0004 OK logout\r\n")
                .await
                .unwrap();
        });

        let mut account = account("localhost");
        account.imap_port = address.port();
        let mut session = super::connect_and_login_with_config(&account, "password", client_config)
            .await
            .unwrap();
        let snapshot = super::fetch_inbox_session(&mut session).await.unwrap();
        assert_eq!(snapshot.headers.len(), 1);
        session.logout().await.unwrap();
        server.await.unwrap();

        // Retain this binding so the test makes it explicit that the client
        // root was built from the server's private test certificate.
        assert!(!certificate.is_empty());
    }

    #[tokio::test]
    async fn self_signed_certificate_is_rejected_before_login() {
        let (_, server_config, _) = test_tls_configs();
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let acceptor = TlsAcceptor::from(Arc::new(server_config));
            let Ok(tls) = acceptor.accept(tcp).await else {
                return None;
            };
            let (reader, mut writer) = tokio::io::split(tls);
            writer.write_all(b"* OK ready\r\n").await.unwrap();
            let mut lines = BufReader::new(reader).lines();
            match timeout(Duration::from_millis(250), lines.next_line()).await {
                Ok(Ok(line)) => line,
                _ => None,
            }
        });

        let mut account = account("localhost");
        account.imap_port = address.port();
        let empty_roots = RootCertStore::empty();
        let client_config = ClientConfig::builder()
            .with_root_certificates(empty_roots)
            .with_no_client_auth();
        assert!(matches!(
            super::connect_and_login_with_config(&account, "password", client_config).await,
            Err(ImapError::Tls)
        ));
        assert!(server.await.unwrap().is_none());
    }

    fn test_tls_configs() -> (ClientConfig, ServerConfig, Vec<u8>) {
        let certified = generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
        let certificate = certified.cert.der().to_vec();
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
            certified.signing_key.serialize_der(),
        ));
        let server = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![CertificateDer::from(certificate.clone())], key)
            .unwrap();
        let mut roots = RootCertStore::empty();
        roots
            .add(CertificateDer::from(certificate.clone()))
            .unwrap();
        let client = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        (client, server, certificate)
    }
}
