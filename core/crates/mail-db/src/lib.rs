//! SQLite-backed local mail store.
//!
//! The database is deliberately expressed in terms of `mail-model` records. The
//! protocol crates can therefore synchronize into this store without exposing
//! IMAP concepts to the Swift application.

use std::{ops::Deref, path::Path};

use mail_model::{
    Account, AttachmentSummary, Mailbox, MailboxSnapshot, MessageDetails, MessageSummary,
    RemoteHeader,
};
use rusqlite::{params, types::Type, Connection, OptionalExtension};

const SCHEMA_VERSION: i64 = 3;
/// Keep one sync snapshot from accidentally turning into an unbounded import.
pub const MAX_SNAPSHOT_HEADERS: usize = 200;
pub const MAX_PAGE_SIZE: u32 = 200;

#[derive(Debug, thiserror::Error)]
pub enum DatabaseError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("unsupported database schema version {0}")]
    UnsupportedSchemaVersion(i64),
    #[error("cannot change remote identity for account {account_id} after mailboxes are cached")]
    RemoteIdentityChange { account_id: String },
    #[error("snapshot contains {actual} headers; maximum is {max}")]
    SnapshotTooLarge { actual: usize, max: usize },
}

pub type Result<T> = std::result::Result<T, DatabaseError>;
pub type Error = DatabaseError;

/// A connection to the local canonical mail store.
pub struct Database {
    connection: Connection,
}

impl Database {
    /// Open (or create) a database and run all pending migrations.
    /// # Errors
    ///
    /// Returns an error when `SQLite` cannot open the path or initialize the
    /// schema.
    pub fn open(path: &str) -> Result<Self> {
        let connection = Connection::open(Path::new(path))?;
        // Foreign keys must be enabled before any transaction starts. WAL is
        // ignored by SQLite for an in-memory database, which is fine for tests.
        connection.execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;")?;
        migrate(&connection)?;
        Ok(Self { connection })
    }

    /// # Errors
    ///
    /// Returns an error when `SQLite` rejects the account or when changing a
    /// cached account's remote identity would invalidate its mailbox cache.
    pub fn upsert_account(&self, account: &Account) -> Result<()> {
        let existing = self
            .connection
            .query_row(
                "SELECT imap_host, imap_port, username FROM accounts WHERE id = ?1",
                [&account.id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        if let Some((imap_host, imap_port, username)) = existing {
            let remote_identity_changed = imap_host != account.imap_host
                || imap_port != i64::from(account.imap_port)
                || username != account.username;
            if remote_identity_changed {
                let has_cached_mailboxes: bool = self.connection.query_row(
                    "SELECT EXISTS(SELECT 1 FROM mailboxes WHERE account_id = ?1)",
                    [&account.id],
                    |row| Ok(row.get::<_, i64>(0)? != 0),
                )?;
                if has_cached_mailboxes {
                    return Err(DatabaseError::RemoteIdentityChange {
                        account_id: account.id.clone(),
                    });
                }
            }
        }
        self.connection.execute(
            "INSERT INTO accounts
                (id, email, display_name, imap_host, imap_port, username, credential_ref)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(id) DO UPDATE SET
                email = excluded.email,
                display_name = excluded.display_name,
                imap_host = excluded.imap_host,
                imap_port = excluded.imap_port,
                username = excluded.username,
                credential_ref = excluded.credential_ref",
            params![
                account.id,
                account.email,
                account.display_name,
                account.imap_host,
                i64::from(account.imap_port),
                account.username,
                account.credential_ref,
            ],
        )?;
        Ok(())
    }

    /// # Errors
    ///
    /// Returns an error when `SQLite` cannot read the account rows.
    pub fn list_accounts(&self) -> Result<Vec<Account>> {
        let mut statement = self.connection.prepare(
            "SELECT id, email, display_name, imap_host, imap_port, username, credential_ref
             FROM accounts ORDER BY id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(Account {
                id: row.get(0)?,
                email: row.get(1)?,
                display_name: row.get(2)?,
                imap_host: row.get(3)?,
                imap_port: u16_from_sql(row.get::<_, i64>(4)?, 4)?,
                username: row.get(5)?,
                credential_ref: row.get(6)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(DatabaseError::from)
    }

    /// # Errors
    ///
    /// Returns an error when `SQLite` cannot read the mailbox rows.
    pub fn list_mailboxes(&self, account_id: &str) -> Result<Vec<Mailbox>> {
        let mut statement = self.connection.prepare(
            "SELECT id, account_id, remote_name, display_name, uid_validity, uid_next
             FROM mailboxes WHERE account_id = ?1 ORDER BY remote_name",
        )?;
        let rows = statement.query_map([account_id], |row| {
            Ok(Mailbox {
                id: row.get(0)?,
                account_id: row.get(1)?,
                remote_name: row.get(2)?,
                display_name: row.get(3)?,
                uid_validity: row
                    .get::<_, Option<i64>>(4)?
                    .map(|value| u32_from_sql(value, 4))
                    .transpose()?,
                uid_next: row
                    .get::<_, Option<i64>>(5)?
                    .map(|value| u32_from_sql(value, 5))
                    .transpose()?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(DatabaseError::from)
    }

    /// Apply one bounded mailbox header snapshot in one transaction.
    ///
    /// A snapshot with the same UIDVALIDITY is additive: absent older headers
    /// remain cached. A changed UIDVALIDITY invalidates only this mailbox's
    /// memberships; message rows that are still referenced by another mailbox
    /// are retained.
    ///
    /// # Errors
    ///
    /// Returns an error when the snapshot is too large, the account is absent,
    /// or `SQLite` cannot commit the snapshot transaction.
    pub fn apply_snapshot(&self, account_id: &str, snapshot: &MailboxSnapshot) -> Result<Mailbox> {
        if snapshot.headers.len() > MAX_SNAPSHOT_HEADERS {
            return Err(DatabaseError::SnapshotTooLarge {
                actual: snapshot.headers.len(),
                max: MAX_SNAPSHOT_HEADERS,
            });
        }
        let transaction = self.connection.unchecked_transaction()?;
        let mailbox_id = mailbox_id(account_id, &snapshot.remote_name);
        let existing = transaction
            .query_row(
                "SELECT id, account_id, remote_name, display_name, uid_validity, uid_next
                 FROM mailboxes WHERE id = ?1",
                [&mailbox_id],
                mailbox_from_row,
            )
            .optional()?;

        let old_uid_validity = existing.as_ref().and_then(|mailbox| mailbox.uid_validity);
        if existing.is_none() {
            transaction.execute(
                "INSERT INTO mailboxes
                    (id, account_id, remote_name, display_name, uid_validity, uid_next)
                 VALUES (?1, ?2, ?3, ?3, ?4, ?5)",
                params![
                    mailbox_id,
                    account_id,
                    snapshot.remote_name,
                    i64::from(snapshot.uid_validity),
                    snapshot.uid_next.map(i64::from),
                ],
            )?;
        } else if existing.as_ref().map(|mailbox| mailbox.account_id.as_str()) != Some(account_id) {
            // This can only happen if a caller supplies a mailbox id generated
            // by an older scheme. Let SQLite report the invariant violation.
            return Err(DatabaseError::Sqlite(rusqlite::Error::InvalidQuery));
        }

        if old_uid_validity != Some(snapshot.uid_validity) {
            invalidate_mailbox_memberships(&transaction, &mailbox_id)?;
        }

        transaction.execute(
            "UPDATE mailboxes SET uid_validity = ?2, uid_next = ?3 WHERE id = ?1",
            params![
                mailbox_id,
                i64::from(snapshot.uid_validity),
                snapshot.uid_next.map(i64::from),
            ],
        )?;

        let mut headers = snapshot.headers.clone();
        headers.sort_by_key(|header| std::cmp::Reverse(header.uid));
        for header in &headers {
            upsert_header(
                &transaction,
                account_id,
                &mailbox_id,
                snapshot.uid_validity,
                header,
            )?;
        }
        transaction.commit()?;

        Ok(Mailbox {
            id: mailbox_id,
            account_id: account_id.to_owned(),
            remote_name: snapshot.remote_name.clone(),
            display_name: snapshot.remote_name.clone(),
            uid_validity: Some(snapshot.uid_validity),
            uid_next: snapshot.uid_next,
        })
    }

    /// List at most [`MAX_PAGE_SIZE`] messages, newest remote UID first.
    ///
    /// # Errors
    ///
    /// Returns an error when `SQLite` cannot read the requested page.
    pub fn list_messages(
        &self,
        mailbox_id: &str,
        offset: u32,
        limit: u32,
    ) -> Result<Vec<MessageSummary>> {
        let limit = limit.min(MAX_PAGE_SIZE);
        let mut statement = self.connection.prepare(
            "SELECT msg.id, mm.mailbox_id, mm.remote_uid,
                    msg.subject, msg.sender, msg.date, msg.snippet,
                    mm.is_read, mm.is_starred, msg.has_attachments
             FROM mailbox_messages AS mm
             JOIN mailboxes AS mb ON mb.id = mm.mailbox_id
             JOIN messages AS msg ON msg.id = mm.message_id
             WHERE mm.mailbox_id = ?1
               AND mb.uid_validity IS NOT NULL
               AND mm.uid_validity = mb.uid_validity
             ORDER BY mm.remote_uid DESC
             LIMIT ?2 OFFSET ?3",
        )?;
        let rows = statement.query_map(
            params![mailbox_id, i64::from(limit), i64::from(offset)],
            |row| {
                Ok(MessageSummary {
                    id: row.get(0)?,
                    mailbox_id: row.get(1)?,
                    remote_uid: u32_from_sql(row.get::<_, i64>(2)?, 2)?,
                    subject: row.get(3)?,
                    sender: row.get(4)?,
                    date: row.get(5)?,
                    snippet: row.get(6)?,
                    is_read: row.get::<_, i64>(7)? != 0,
                    is_starred: row.get::<_, i64>(8)? != 0,
                    has_attachments: row.get::<_, i64>(9)? != 0,
                })
            },
        )?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(DatabaseError::from)
    }

    /// Read complete cached metadata for one message without network access.
    ///
    /// # Errors
    ///
    /// Returns an error when `SQLite` cannot read the requested message.
    pub fn message_details(&self, message_id: &str) -> Result<Option<MessageDetails>> {
        let base = self
            .connection
            .query_row(
                "SELECT message_id_header, sent_at FROM messages WHERE id = ?1",
                [message_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((message_id_header, sent_at)) = base else {
            return Ok(None);
        };

        let values = |category| detail_values(&self.connection, message_id, category);
        let mut statement = self.connection.prepare(
            "SELECT id, filename, mime_type, size
             FROM message_attachments
             WHERE message_id = ?1
             ORDER BY position",
        )?;
        let attachments = statement
            .query_map([message_id], |row| {
                Ok(AttachmentSummary {
                    id: row.get(0)?,
                    filename: row.get(1)?,
                    mime_type: row.get(2)?,
                    size: u64_from_sql(row.get::<_, i64>(3)?, 3)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(Some(MessageDetails {
            message_id: message_id_header,
            senders: values("from")?,
            recipients: values("to")?,
            cc: values("cc")?,
            bcc: values("bcc")?,
            reply_to: values("reply-to")?,
            sent_at,
            in_reply_to: values("in-reply-to")?,
            references: values("references")?,
            list_id: values("list-id")?,
            list_post: values("list-post")?,
            list_unsubscribe: values("list-unsubscribe")?,
            authentication_results: values("authentication-results")?,
            received_spf: values("received-spf")?,
            attachments,
        }))
    }
}

fn migrate(connection: &Connection) -> Result<()> {
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version > SCHEMA_VERSION {
        return Err(DatabaseError::UnsupportedSchemaVersion(version));
    }
    if version == 0 {
        connection.execute_batch(
            "BEGIN IMMEDIATE;
             CREATE TABLE accounts (
                 id TEXT PRIMARY KEY NOT NULL,
                 email TEXT NOT NULL,
                 display_name TEXT NOT NULL,
                 imap_host TEXT NOT NULL,
                 imap_port INTEGER NOT NULL,
                 username TEXT NOT NULL,
                 credential_ref TEXT NOT NULL
             );
             CREATE TABLE mailboxes (
                 id TEXT PRIMARY KEY NOT NULL,
                 account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
                 remote_name TEXT NOT NULL,
                 display_name TEXT NOT NULL,
                 uid_validity INTEGER,
                 uid_next INTEGER,
                 UNIQUE(account_id, remote_name)
             );
             CREATE TABLE messages (
                 id TEXT PRIMARY KEY NOT NULL,
                 account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
                 message_id_header TEXT,
                 subject TEXT NOT NULL,
                 sender TEXT NOT NULL,
                 date TEXT NOT NULL,
                 snippet TEXT NOT NULL DEFAULT '',
                 sent_at INTEGER,
                 has_attachments INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE mailbox_messages (
                 mailbox_id TEXT NOT NULL REFERENCES mailboxes(id) ON DELETE CASCADE,
                 message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
                 uid_validity INTEGER NOT NULL,
                 remote_uid INTEGER NOT NULL,
                 is_read INTEGER NOT NULL DEFAULT 0,
                 is_starred INTEGER NOT NULL DEFAULT 0,
                 PRIMARY KEY(mailbox_id, uid_validity, remote_uid)
             );
             CREATE INDEX mailbox_messages_page_idx
                 ON mailbox_messages(mailbox_id, uid_validity, remote_uid DESC);
             CREATE INDEX messages_account_idx ON messages(account_id);
             CREATE INDEX mailbox_messages_message_id_idx ON mailbox_messages(message_id);
             CREATE TABLE message_detail_values (
                 message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
                 category TEXT NOT NULL,
                 position INTEGER NOT NULL,
                 value TEXT NOT NULL,
                 PRIMARY KEY(message_id, category, position)
             );
             CREATE TABLE message_attachments (
                 message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
                 position INTEGER NOT NULL,
                 id TEXT NOT NULL,
                 filename TEXT NOT NULL,
                 mime_type TEXT NOT NULL,
                 size INTEGER NOT NULL,
                 PRIMARY KEY(message_id, position)
             );
             PRAGMA user_version = 3;
             COMMIT;",
        )?;
    } else {
        if version == 1 {
            connection.execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE INDEX IF NOT EXISTS mailbox_messages_message_id_idx
                     ON mailbox_messages(message_id);
                 PRAGMA user_version = 2;
                 COMMIT;",
            )?;
        }
        if version <= 2 {
            connection.execute_batch(
                "BEGIN IMMEDIATE;
                 ALTER TABLE messages ADD COLUMN sent_at INTEGER;
                 CREATE TABLE message_detail_values (
                     message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
                     category TEXT NOT NULL,
                     position INTEGER NOT NULL,
                     value TEXT NOT NULL,
                     PRIMARY KEY(message_id, category, position)
                 );
                 CREATE TABLE message_attachments (
                     message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
                     position INTEGER NOT NULL,
                     id TEXT NOT NULL,
                     filename TEXT NOT NULL,
                     mime_type TEXT NOT NULL,
                     size INTEGER NOT NULL,
                     PRIMARY KEY(message_id, position)
                 );
                 PRAGMA user_version = 3;
                 COMMIT;",
            )?;
        }
    }
    Ok(())
}

fn mailbox_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Mailbox> {
    Ok(Mailbox {
        id: row.get(0)?,
        account_id: row.get(1)?,
        remote_name: row.get(2)?,
        display_name: row.get(3)?,
        uid_validity: row
            .get::<_, Option<i64>>(4)?
            .map(|value| u32_from_sql(value, 4))
            .transpose()?,
        uid_next: row
            .get::<_, Option<i64>>(5)?
            .map(|value| u32_from_sql(value, 5))
            .transpose()?,
    })
}

fn u16_from_sql(value: i64, column: usize) -> rusqlite::Result<u16> {
    u16::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(column, Type::Integer, Box::new(error))
    })
}

fn u32_from_sql(value: i64, column: usize) -> rusqlite::Result<u32> {
    u32::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(column, Type::Integer, Box::new(error))
    })
}

fn u64_from_sql(value: i64, column: usize) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(column, Type::Integer, Box::new(error))
    })
}

fn detail_values(connection: &Connection, message_id: &str, category: &str) -> Result<Vec<String>> {
    let mut statement = connection.prepare(
        "SELECT value FROM message_detail_values
         WHERE message_id = ?1 AND category = ?2
         ORDER BY position",
    )?;
    let values = statement.query_map(params![message_id, category], |row| row.get(0))?;
    values
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(DatabaseError::from)
}

fn invalidate_mailbox_memberships<T>(transaction: &T, mailbox_id: &str) -> Result<()>
where
    T: Deref<Target = Connection>,
{
    let mut statement =
        transaction.prepare("SELECT message_id FROM mailbox_messages WHERE mailbox_id = ?1")?;
    let ids = statement
        .query_map([mailbox_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);

    transaction.execute(
        "DELETE FROM mailbox_messages WHERE mailbox_id = ?1",
        [mailbox_id],
    )?;
    for message_id in ids {
        transaction.execute(
            "DELETE FROM messages
             WHERE id = ?1
               AND NOT EXISTS (SELECT 1 FROM mailbox_messages WHERE message_id = ?1)",
            [&message_id],
        )?;
    }
    Ok(())
}

fn upsert_header<T>(
    transaction: &T,
    account_id: &str,
    mailbox_id: &str,
    uid_validity: u32,
    header: &RemoteHeader,
) -> Result<()>
where
    T: Deref<Target = Connection>,
{
    let message_row_id = message_id(mailbox_id, uid_validity, header.uid);
    let has_attachments = header
        .content
        .as_ref()
        .map(|content| i64::from(u8::from(!content.attachments.is_empty())));
    transaction.execute(
        "INSERT INTO messages
            (id, account_id, message_id_header, subject, sender, date, snippet, sent_at,
             has_attachments)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, '', ?7, COALESCE(?8, 0))
         ON CONFLICT(id) DO UPDATE SET
            account_id = excluded.account_id,
            message_id_header = excluded.message_id_header,
            subject = excluded.subject,
            sender = excluded.sender,
            date = excluded.date,
            sent_at = excluded.sent_at,
            has_attachments = COALESCE(?8, messages.has_attachments)",
        params![
            message_row_id,
            account_id,
            header.message_id,
            header.subject,
            header.sender,
            header.date,
            header.sent_at,
            has_attachments,
        ],
    )?;

    transaction.execute(
        "DELETE FROM message_detail_values WHERE message_id = ?1",
        [&message_row_id],
    )?;
    for (category, values) in [
        ("from", &header.senders),
        ("to", &header.recipients),
        ("cc", &header.cc),
        ("bcc", &header.bcc),
        ("reply-to", &header.reply_to),
        ("in-reply-to", &header.in_reply_to),
        ("references", &header.references),
        ("list-id", &header.list_id),
        ("list-post", &header.list_post),
        ("list-unsubscribe", &header.list_unsubscribe),
        ("authentication-results", &header.authentication_results),
        ("received-spf", &header.received_spf),
    ] {
        for (position, value) in values.iter().enumerate() {
            let position =
                i64::try_from(position).map_err(|_| rusqlite::Error::InvalidQuery)?;
            transaction.execute(
                "INSERT INTO message_detail_values (message_id, category, position, value)
                 VALUES (?1, ?2, ?3, ?4)",
                params![message_row_id, category, position, value],
            )?;
        }
    }

    if let Some(content) = &header.content {
        transaction.execute(
            "DELETE FROM message_attachments WHERE message_id = ?1",
            [&message_row_id],
        )?;
        for (position, attachment) in content.attachments.iter().enumerate() {
            let position =
                i64::try_from(position).map_err(|_| rusqlite::Error::InvalidQuery)?;
            let size = i64::try_from(attachment.data.len())
                .map_err(|_| rusqlite::Error::InvalidQuery)?;
            transaction.execute(
                "INSERT INTO message_attachments
                    (message_id, position, id, filename, mime_type, size)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    message_row_id,
                    position,
                    attachment.id,
                    attachment.filename,
                    attachment.mime_type,
                    size,
                ],
            )?;
        }
    }
    transaction.execute(
        "INSERT INTO mailbox_messages
            (mailbox_id, message_id, uid_validity, remote_uid, is_read, is_starred)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(mailbox_id, uid_validity, remote_uid) DO UPDATE SET
            message_id = excluded.message_id,
            is_read = excluded.is_read,
            is_starred = excluded.is_starred",
        params![
            mailbox_id,
            message_row_id,
            i64::from(uid_validity),
            i64::from(header.uid),
            i64::from(u8::from(header.is_read)),
            i64::from(u8::from(header.is_starred)),
        ],
    )?;
    Ok(())
}

fn mailbox_id(account_id: &str, remote_name: &str) -> String {
    // Length-prefixing makes concatenation unambiguous even when either value
    // contains separators or Unicode characters.
    format!(
        "mbx:{}:{}:{}:{}",
        account_id.len(),
        account_id,
        remote_name.len(),
        remote_name
    )
}

fn message_id(mailbox_id: &str, uid_validity: u32, remote_uid: u32) -> String {
    format!(
        "msg:{}:{}:{}:{}",
        mailbox_id.len(),
        mailbox_id,
        uid_validity,
        remote_uid
    )
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn account(id: &str) -> Account {
        Account {
            id: id.into(),
            email: format!("{id}@example.test"),
            display_name: id.into(),
            imap_host: "imap.example.test".into(),
            imap_port: 993,
            username: id.into(),
            credential_ref: format!("keychain://{id}"),
        }
    }

    fn snapshot(name: &str, validity: u32, uids: &[u32]) -> MailboxSnapshot {
        MailboxSnapshot {
            remote_name: name.into(),
            uid_validity: validity,
            uid_next: uids.iter().max().map(|uid| uid + 1),
            headers: uids
                .iter()
                .map(|uid| RemoteHeader {
                    uid: *uid,
                    message_id: Some(format!("<{uid}@example.test>")),
                    subject: format!("Subject {uid}"),
                    sender: format!("sender{uid}@example.test"),
                    date: format!("2026-01-{uid:02}"),
                    is_read: uid % 2 == 0,
                    is_starred: uid % 3 == 0,
                    ..RemoteHeader::default()
                })
                .collect(),
        }
    }

    #[test]
    fn persists_across_reopen_and_is_offline_readable() {
        let file = NamedTempFile::new().unwrap();
        let path = file.path().to_str().unwrap().to_owned();
        {
            let db = Database::open(&path).unwrap();
            db.upsert_account(&account("one")).unwrap();
            db.apply_snapshot("one", &snapshot("INBOX", 1, &[1, 2]))
                .unwrap();
        }
        let db = Database::open(&path).unwrap();
        let mailbox = db.list_mailboxes("one").unwrap().pop().unwrap();
        let messages = db.list_messages(&mailbox.id, 0, 10).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].remote_uid, 2);
    }

    #[test]
    fn migrates_existing_cache_without_losing_messages() {
        let file = NamedTempFile::new().unwrap();
        let path = file.path().to_str().unwrap();
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE messages (
                     id TEXT PRIMARY KEY NOT NULL,
                     account_id TEXT NOT NULL,
                     message_id_header TEXT,
                     subject TEXT NOT NULL,
                     sender TEXT NOT NULL,
                     date TEXT NOT NULL,
                     snippet TEXT NOT NULL DEFAULT '',
                     has_attachments INTEGER NOT NULL DEFAULT 0
                 );
                 INSERT INTO messages
                    (id, account_id, message_id_header, subject, sender, date)
                 VALUES ('old-message', 'old-account', 'old@example.test', 'Old', 'Sender', '2026');
                 PRAGMA user_version = 2;",
            )
            .unwrap();
        drop(connection);

        let db = Database::open(path).unwrap();
        let details = db.message_details("old-message").unwrap().unwrap();
        assert_eq!(details.message_id.as_deref(), Some("old@example.test"));
        assert!(details.recipients.is_empty());
        assert!(details.attachments.is_empty());
    }

    #[test]
    fn persists_complete_message_details_and_attachment_metadata() {
        let db = Database::open(":memory:").unwrap();
        db.upsert_account(&account("one")).unwrap();
        let mut snapshot = snapshot("INBOX", 1, &[4]);
        let header = &mut snapshot.headers[0];
        header.recipients = vec!["To <to@example.test>".into()];
        header.senders = vec!["First <first@example.test>".into(), "second@example.test".into()];
        header.cc = vec!["cc@example.test".into()];
        header.bcc = vec!["bcc@example.test".into()];
        header.reply_to = vec!["reply@example.test".into()];
        header.sent_at = Some(1_789_713_000);
        header.in_reply_to = vec!["parent@example.test".into()];
        header.references = vec!["root@example.test".into(), "parent@example.test".into()];
        header.list_id = vec!["Tern <list.tern.example>".into()];
        header.list_post = vec!["mailto:list@tern.example".into()];
        header.list_unsubscribe = vec!["https://tern.example/leave".into()];
        header.authentication_results = vec!["mx.example; dkim=pass".into()];
        header.received_spf = vec!["pass".into()];
        header.content = Some(mail_model::MessageContent {
            attachments: vec![mail_model::Attachment {
                id: "part-1".into(),
                filename: "report.pdf".into(),
                mime_type: "application/pdf".into(),
                data: vec![1, 2, 3, 4],
                content_id: None,
            }],
            ..mail_model::MessageContent::default()
        });
        db.apply_snapshot("one", &snapshot).unwrap();

        let mailbox = db.list_mailboxes("one").unwrap().pop().unwrap();
        let message = db.list_messages(&mailbox.id, 0, 1).unwrap().pop().unwrap();
        assert!(message.has_attachments);
        let details = db.message_details(&message.id).unwrap().unwrap();
        assert_eq!(details.senders.len(), 2);
        assert_eq!(details.recipients, vec!["To <to@example.test>"]);
        assert_eq!(details.reply_to, vec!["reply@example.test"]);
        assert_eq!(details.references.len(), 2);
        assert_eq!(details.authentication_results, vec!["mx.example; dkim=pass"]);
        assert_eq!(details.attachments.len(), 1);
        assert_eq!(details.attachments[0].filename, "report.pdf");
        assert_eq!(details.attachments[0].size, 4);
    }

    #[test]
    fn header_only_refresh_preserves_cached_attachment_metadata() {
        let db = Database::open(":memory:").unwrap();
        db.upsert_account(&account("one")).unwrap();
        let mut complete = snapshot("INBOX", 1, &[1]);
        complete.headers[0].content = Some(mail_model::MessageContent {
            attachments: vec![mail_model::Attachment {
                id: "part-1".into(),
                filename: "cached.txt".into(),
                mime_type: "text/plain".into(),
                data: b"cached".to_vec(),
                content_id: None,
            }],
            ..mail_model::MessageContent::default()
        });
        db.apply_snapshot("one", &complete).unwrap();
        db.apply_snapshot("one", &snapshot("INBOX", 1, &[1]))
            .unwrap();

        let mailbox = db.list_mailboxes("one").unwrap().pop().unwrap();
        let message = db.list_messages(&mailbox.id, 0, 1).unwrap().pop().unwrap();
        let details = db.message_details(&message.id).unwrap().unwrap();
        assert!(message.has_attachments);
        assert_eq!(details.attachments[0].filename, "cached.txt");
    }

    #[test]
    fn same_snapshot_is_idempotent_and_keeps_absent_older_headers() {
        let db = Database::open(":memory:").unwrap();
        db.upsert_account(&account("one")).unwrap();
        db.apply_snapshot("one", &snapshot("INBOX", 1, &[1, 2, 3]))
            .unwrap();
        db.apply_snapshot("one", &snapshot("INBOX", 1, &[3]))
            .unwrap();
        let mailbox = db.list_mailboxes("one").unwrap().pop().unwrap();
        assert_eq!(db.list_messages(&mailbox.id, 0, 20).unwrap().len(), 3);
    }

    #[test]
    fn uid_validity_change_resets_only_that_mailbox() {
        let db = Database::open(":memory:").unwrap();
        db.upsert_account(&account("one")).unwrap();
        db.upsert_account(&account("two")).unwrap();
        db.apply_snapshot("one", &snapshot("INBOX", 1, &[1, 2]))
            .unwrap();
        db.apply_snapshot("one", &snapshot("Archive", 1, &[7]))
            .unwrap();
        db.apply_snapshot("two", &snapshot("INBOX", 1, &[9]))
            .unwrap();
        db.apply_snapshot("one", &snapshot("INBOX", 2, &[5]))
            .unwrap();
        let one_inbox = db
            .list_mailboxes("one")
            .unwrap()
            .into_iter()
            .find(|mailbox| mailbox.remote_name == "INBOX")
            .unwrap();
        let one_archive = db
            .list_mailboxes("one")
            .unwrap()
            .into_iter()
            .find(|mailbox| mailbox.remote_name == "Archive")
            .unwrap();
        let two_inbox = db.list_mailboxes("two").unwrap().pop().unwrap();
        assert_eq!(
            db.list_messages(&one_inbox.id, 0, 20).unwrap()[0].remote_uid,
            5
        );
        assert_eq!(
            db.list_messages(&one_archive.id, 0, 20).unwrap()[0].remote_uid,
            7
        );
        assert_eq!(
            db.list_messages(&two_inbox.id, 0, 20).unwrap()[0].remote_uid,
            9
        );
    }

    #[test]
    fn pagination_is_bounded() {
        let db = Database::open(":memory:").unwrap();
        db.upsert_account(&account("one")).unwrap();
        let first_batch = (1..=200).collect::<Vec<_>>();
        let second_batch = (201..=250).collect::<Vec<_>>();
        db.apply_snapshot("one", &snapshot("INBOX", 1, &first_batch))
            .unwrap();
        db.apply_snapshot("one", &snapshot("INBOX", 1, &second_batch))
            .unwrap();
        let mailbox = db.list_mailboxes("one").unwrap().pop().unwrap();
        assert_eq!(db.list_messages(&mailbox.id, 0, 10_000).unwrap().len(), 200);
    }

    #[test]
    fn oversized_snapshot_is_rejected_without_changing_cached_rows() {
        let db = Database::open(":memory:").unwrap();
        db.upsert_account(&account("one")).unwrap();
        db.apply_snapshot("one", &snapshot("INBOX", 1, &[1, 2]))
            .unwrap();
        let mailbox = db.list_mailboxes("one").unwrap().pop().unwrap();
        let oversized_uids =
            (1..=(u32::try_from(MAX_SNAPSHOT_HEADERS).unwrap() + 1)).collect::<Vec<_>>();
        let oversized = snapshot("INBOX", 1, &oversized_uids);

        let error = db.apply_snapshot("one", &oversized).unwrap_err();
        assert!(matches!(
            error,
            DatabaseError::SnapshotTooLarge {
                actual,
                max: MAX_SNAPSHOT_HEADERS
            } if actual == MAX_SNAPSHOT_HEADERS + 1
        ));
        let messages = db.list_messages(&mailbox.id, 0, 20).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].remote_uid, 2);
    }

    #[test]
    fn foreign_key_failure_rolls_back_mailbox_insert() {
        let db = Database::open(":memory:").unwrap();
        let error = db
            .apply_snapshot("missing-account", &snapshot("INBOX", 1, &[1]))
            .unwrap_err();
        assert!(matches!(error, DatabaseError::Sqlite(_)));
        assert!(db.list_mailboxes("missing-account").unwrap().is_empty());
    }

    #[test]
    fn duplicate_message_ids_remain_distinct_by_uid() {
        let db = Database::open(":memory:").unwrap();
        db.upsert_account(&account("one")).unwrap();
        let mut snapshot = snapshot("INBOX", 1, &[1, 2]);
        snapshot.headers[1].message_id = snapshot.headers[0].message_id.clone();
        db.apply_snapshot("one", &snapshot).unwrap();
        let mailbox = db.list_mailboxes("one").unwrap().pop().unwrap();
        let messages = db.list_messages(&mailbox.id, 0, 20).unwrap();
        assert_eq!(messages.len(), 2);
        assert_ne!(messages[0].id, messages[1].id);
        assert_ne!(messages[0].remote_uid, messages[1].remote_uid);
    }

    #[test]
    fn cached_mailboxes_reject_remote_identity_change() {
        let db = Database::open(":memory:").unwrap();
        db.upsert_account(&account("one")).unwrap();
        db.apply_snapshot("one", &snapshot("INBOX", 1, &[1]))
            .unwrap();
        let mut changed = account("one");
        changed.imap_host = "other.example.test".into();

        let error = db.upsert_account(&changed).unwrap_err();
        assert!(matches!(
            error,
            DatabaseError::RemoteIdentityChange { account_id } if account_id == "one"
        ));
        assert_eq!(
            db.list_accounts().unwrap()[0].imap_host,
            "imap.example.test"
        );
    }
}
