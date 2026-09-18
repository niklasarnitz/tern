//! Read-only IMAP synchronization for the first vertical slice.
//!
//! Connections use implicit TLS on port 993 and the platform certificate
//! store.  This crate intentionally does not offer STARTTLS, certificate
//! overrides, or password authentication for Gmail; those choices keep the
//! initial network boundary small and safe while OAuth support is added.

use std::{convert::TryFrom, fmt::Debug, sync::Arc, time::Duration};

use async_imap::{
    types::{Flag, NameAttribute},
    Client, Session,
};
use futures_util::TryStreamExt;
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
const MAX_HEADERS: u32 = 100;

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

    let snapshot = fetch_inbox_session(&mut session).await?;
    let _ = timeout(OPERATION_TIMEOUT, session.logout()).await;
    Ok(snapshot)
}

async fn fetch_inbox_session<T>(session: &mut Session<T>) -> Result<MailboxSnapshot>
where
    T: AsyncRead + AsyncWrite + Unpin + Debug + Send,
{
    // EXAMINE keeps this first slice strictly read-only.  It does not mark
    // recent messages as seen and cannot mutate mailbox state.
    let mailbox = timeout(OPERATION_TIMEOUT, session.examine("INBOX"))
        .await
        .map_err(|_| ImapError::Timeout)?
        .map_err(|_| ImapError::Protocol)?;
    let uid_validity = mailbox.uid_validity.ok_or(ImapError::MissingUidMetadata)?;

    let headers = if mailbox.exists == 0 {
        Vec::new()
    } else {
        let first_sequence = mailbox.exists.saturating_sub(MAX_HEADERS - 1).max(1);
        let sequence_set = format!("{first_sequence}:{}", mailbox.exists);
        let mut stream = timeout(
            OPERATION_TIMEOUT,
            session.fetch(
                sequence_set,
                "(UID FLAGS INTERNALDATE BODY.PEEK[HEADER.FIELDS (MESSAGE-ID SUBJECT FROM DATE)])",
            ),
        )
        .await
        .map_err(|_| ImapError::Timeout)?
        .map_err(|_| ImapError::Protocol)?;

        let mut headers = Vec::with_capacity(MAX_HEADERS as usize);
        while let Some(fetch) = timeout(OPERATION_TIMEOUT, stream.try_next())
            .await
            .map_err(|_| ImapError::Timeout)?
            .map_err(|_| ImapError::Protocol)?
        {
            let uid = fetch.uid.ok_or(ImapError::MissingUidMetadata)?;
            let is_read = fetch.flags().any(|flag| matches!(flag, Flag::Seen));
            let is_starred = fetch.flags().any(|flag| matches!(flag, Flag::Flagged));
            let mut header = mail_mime::parse_header(
                uid,
                fetch.header().unwrap_or_default(),
                is_read,
                is_starred,
            );

            // INTERNALDATE describes the server's arrival time and is the
            // stable fallback for messages whose Date header is absent.
            if let Some(internal_date) = fetch.internal_date() {
                header.date = internal_date.to_rfc3339();
            }
            headers.push(header);
            if headers.len() == MAX_HEADERS as usize {
                break;
            }
        }

        // UIDs generally increase with arrival.  Sorting makes the result
        // deterministic even when a server emits FETCH responses out of order.
        headers.sort_by_key(|header| std::cmp::Reverse(header.uid));
        headers
    };

    Ok(MailboxSnapshot {
        remote_name: "INBOX".to_owned(),
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
    let mut stream = timeout(OPERATION_TIMEOUT, session.list(None, Some("*")))
        .await
        .map_err(|_| ImapError::Timeout)?
        .map_err(|_| ImapError::Protocol)?;

    let mut names = Vec::new();
    while let Some(name) = timeout(OPERATION_TIMEOUT, stream.try_next())
        .await
        .map_err(|_| ImapError::Timeout)?
        .map_err(|_| ImapError::Protocol)?
    {
        if !name
            .attributes()
            .iter()
            .any(|attribute| matches!(attribute, NameAttribute::NoSelect))
        {
            names.push(name.name().to_owned());
        }
    }
    drop(stream);

    let _ = timeout(OPERATION_TIMEOUT, session.logout()).await;
    Ok(names)
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
    let host = account.imap_host.trim_end_matches('.');
    let tcp = timeout(
        CONNECT_TIMEOUT,
        TcpStream::connect((host, account.imap_port)),
    )
    .await
    .map_err(|_| ImapError::Timeout)?
    .map_err(|_| ImapError::Connection)?;

    let config = tls_config()?;
    let connector = TlsConnector::from(Arc::new(config));
    let server_name =
        ServerName::try_from(host.to_owned()).map_err(|_| ImapError::InvalidConfiguration)?;
    let tls_stream = timeout(CONNECT_TIMEOUT, connector.connect(server_name, tcp))
        .await
        .map_err(|_| ImapError::Timeout)?
        .map_err(|_| ImapError::Tls)?;

    let mut client = Client::new(tls_stream);
    let greeting = timeout(OPERATION_TIMEOUT, client.read_response())
        .await
        .map_err(|_| ImapError::Timeout)?
        .map_err(|_| ImapError::Protocol)?;
    if greeting.is_none() {
        return Err(ImapError::Connection);
    }

    timeout(OPERATION_TIMEOUT, client.login(&account.username, password))
        .await
        .map_err(|_| ImapError::Timeout)?
        .map_err(|_| ImapError::Authentication)
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
    use std::time::Duration;

    use super::{validate_account, ImapError};
    use async_imap::Client;
    use mail_model::Account;

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
    fn requires_implicit_tls_port() {
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
}
