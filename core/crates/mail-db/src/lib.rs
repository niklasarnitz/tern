//! SQLite-backed local mail store.
//!
//! The database is deliberately expressed in terms of `mail-model` records. The
//! protocol crates can therefore synchronize into this store without exposing
//! IMAP concepts to the Swift application.

use std::{collections::HashMap, ops::Deref, path::Path};

use mail_model::{
    Account, AttachmentSummary, Mailbox, MailboxSnapshot, MessageContent, MessageDetails,
    MessageSummary, RemoteHeader, WidgetMailboxSummary, WidgetSnapshot,
};
use rusqlite::{
    params, params_from_iter, types::Type, types::Value, Connection, OptionalExtension,
};

mod conversations;
mod mutations;
mod search;

const SCHEMA_VERSION: i64 = 5;
/// Keep one sync snapshot from accidentally turning into an unbounded import.
pub const MAX_SNAPSHOT_HEADERS: usize = 200;
pub const MAX_PAGE_SIZE: u32 = 200;
pub const MAX_WIDGET_MAILBOXES: usize = 20;
pub const MAX_WIDGET_MESSAGES: u32 = 10;

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
    #[error("message {message_id} is not in mailbox {mailbox_id}")]
    MessageNotInMailbox {
        message_id: String,
        mailbox_id: String,
    },
    #[error("thread {thread_id} has no messages in mailbox {mailbox_id}")]
    ThreadNotInMailbox {
        thread_id: String,
        mailbox_id: String,
    },
    #[error("move requires an existing destination mailbox")]
    InvalidMoveDestination,
    #[error("pending operation {0} does not exist")]
    PendingOperationNotFound(String),
    #[error("mail data could not be serialized: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("invalid search: {message}")]
    InvalidSearch { message: String },
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
                    mm.is_read, mm.is_starred, msg.has_attachments, mm.local_only
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
                let local_only = row.get::<_, i64>(10)? != 0;
                Ok(MessageSummary {
                    id: row.get(0)?,
                    mailbox_id: row.get(1)?,
                    remote_uid: if local_only {
                        0
                    } else {
                        u32_from_sql(row.get::<_, i64>(2)?, 2)?
                    },
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
    /// Returns an error when `SQLite` cannot read or decode the message.
    pub fn message_details(&self, message_id: &str) -> Result<Option<MessageDetails>> {
        let stored = self
            .connection
            .query_row(
                "SELECT message_id_header, senders_json, recipients_json, cc_json, bcc_json,
                        reply_to_json, sent_at, in_reply_to_json, references_json, list_id_json,
                        list_post_json, list_unsubscribe_json, authentication_results_json,
                        received_spf_json, content_json
                 FROM messages WHERE id = ?1",
                [message_id],
                stored_message_details,
            )
            .optional()?;
        stored.map(StoredMessageDetails::decode).transpose()
    }

    /// Search the local cache with free text and structured operators.
    ///
    /// Free text and `from:`, `to:`, and `subject:` use the FTS index. The
    /// indexed predicates are `before:YYYY-MM-DD`, `after:YYYY-MM-DD`,
    /// `has:attachment`, `is:unread`, `is:starred`, `in:`, and `account:`.
    /// Results include at most [`MAX_PAGE_SIZE`] logical messages. Values
    /// containing spaces can be enclosed in double quotes.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed dates or quoted values, or when `SQLite`
    /// cannot execute the search.
    pub fn search_messages(
        &self,
        query: &str,
        offset: u32,
        limit: u32,
    ) -> Result<Vec<MessageSummary>> {
        let plan = search::parse(query)?;
        let limit = limit.min(MAX_PAGE_SIZE);
        let mut sql = String::from(
            "SELECT msg.id, MIN(mm.mailbox_id),
                    MAX(CASE WHEN mm.local_only = 1 THEN 0 ELSE mm.remote_uid END),
                    msg.subject, msg.sender, msg.date, msg.snippet,
                    MIN(mm.is_read), MAX(mm.is_starred), msg.has_attachments
             FROM mailbox_messages AS mm
             JOIN mailboxes AS mb ON mb.id = mm.mailbox_id
             JOIN messages AS msg ON msg.id = mm.message_id
             JOIN accounts AS acc ON acc.id = msg.account_id",
        );
        let mut values = Vec::new();
        if let Some(fts_query) = &plan.fts_query {
            sql.push_str(" JOIN messages_fts ON messages_fts.rowid = msg.rowid");
            sql.push_str(
                " WHERE mb.uid_validity IS NOT NULL
                    AND mm.uid_validity = mb.uid_validity
                    AND messages_fts MATCH ?",
            );
            values.push(Value::Text(fts_query.clone()));
        } else {
            sql.push_str(
                " WHERE mb.uid_validity IS NOT NULL
                    AND mm.uid_validity = mb.uid_validity",
            );
        }
        for timestamp in plan.before {
            sql.push_str(" AND msg.sent_at < ?");
            values.push(Value::Integer(timestamp));
        }
        for timestamp in plan.after {
            sql.push_str(" AND msg.sent_at >= ?");
            values.push(Value::Integer(timestamp));
        }
        if plan.has_attachments {
            sql.push_str(" AND msg.has_attachments = 1");
        }
        if plan.is_unread {
            sql.push_str(" AND mm.is_read = 0");
        }
        if plan.is_starred {
            sql.push_str(" AND mm.is_starred = 1");
        }
        append_text_filter(
            &mut sql,
            &mut values,
            &plan.mailboxes,
            &["mb.remote_name", "mb.display_name"],
        );
        append_text_filter(
            &mut sql,
            &mut values,
            &plan.accounts,
            &["acc.id", "acc.email", "acc.display_name"],
        );
        sql.push_str(" GROUP BY msg.id");
        sql.push_str(" ORDER BY COALESCE(msg.sent_at, 0) DESC, msg.id");
        sql.push_str(" LIMIT ? OFFSET ?");
        values.push(Value::Integer(i64::from(limit)));
        values.push(Value::Integer(i64::from(offset)));

        let mut statement = self.connection.prepare(&sql)?;
        let rows =
            statement.query_map(params_from_iter(values.iter()), message_summary_from_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(DatabaseError::from)
    }
}

fn append_text_filter(
    sql: &mut String,
    parameters: &mut Vec<Value>,
    values: &[String],
    columns: &[&str],
) {
    if values.is_empty() {
        return;
    }
    sql.push_str(" AND (");
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            sql.push_str(" OR ");
        }
        sql.push('(');
        for (column_index, column) in columns.iter().enumerate() {
            if column_index > 0 {
                sql.push_str(" OR ");
            }
            sql.push_str(column);
            sql.push_str(" = ? COLLATE NOCASE");
            parameters.push(Value::Text(value.clone()));
        }
        sql.push(')');
    }
    sql.push(')');
}

fn message_summary_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MessageSummary> {
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
}

struct StoredMessageDetails {
    message_id: Option<String>,
    senders: String,
    recipients: String,
    cc: String,
    bcc: String,
    reply_to: String,
    sent_at: Option<i64>,
    in_reply_to: String,
    references: String,
    list_id: String,
    list_post: String,
    list_unsubscribe: String,
    authentication_results: String,
    received_spf: String,
    content: Option<String>,
}

fn stored_message_details(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredMessageDetails> {
    Ok(StoredMessageDetails {
        message_id: row.get(0)?,
        senders: row.get(1)?,
        recipients: row.get(2)?,
        cc: row.get(3)?,
        bcc: row.get(4)?,
        reply_to: row.get(5)?,
        sent_at: row.get(6)?,
        in_reply_to: row.get(7)?,
        references: row.get(8)?,
        list_id: row.get(9)?,
        list_post: row.get(10)?,
        list_unsubscribe: row.get(11)?,
        authentication_results: row.get(12)?,
        received_spf: row.get(13)?,
        content: row.get(14)?,
    })
}

impl StoredMessageDetails {
    fn decode(self) -> Result<MessageDetails> {
        let content = self
            .content
            .as_deref()
            .map(serde_json::from_str::<MessageContent>)
            .transpose()?;
        let attachments = content
            .map(|content| {
                content
                    .attachments
                    .into_iter()
                    .map(|attachment| {
                        Ok(AttachmentSummary {
                            id: attachment.id,
                            filename: attachment.filename,
                            mime_type: attachment.mime_type,
                            size: u64::try_from(attachment.data.len())
                                .map_err(|_| rusqlite::Error::InvalidQuery)?,
                        })
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default();
        Ok(MessageDetails {
            message_id: self.message_id,
            senders: serde_json::from_str(&self.senders)?,
            recipients: serde_json::from_str(&self.recipients)?,
            cc: serde_json::from_str(&self.cc)?,
            bcc: serde_json::from_str(&self.bcc)?,
            reply_to: serde_json::from_str(&self.reply_to)?,
            sent_at: self.sent_at,
            in_reply_to: serde_json::from_str(&self.in_reply_to)?,
            references: serde_json::from_str(&self.references)?,
            list_id: serde_json::from_str(&self.list_id)?,
            list_post: serde_json::from_str(&self.list_post)?,
            list_unsubscribe: serde_json::from_str(&self.list_unsubscribe)?,
            authentication_results: serde_json::from_str(&self.authentication_results)?,
            received_spf: serde_json::from_str(&self.received_spf)?,
            attachments,
        })
    }
}

impl Database {
    /// Read a bounded summary for widgets from explicitly selected mailboxes.
    ///
    /// Unknown mailbox ids are ignored. An empty selection returns no counts
    /// or message metadata, so a widget cannot accidentally broaden its scope.
    ///
    /// # Errors
    ///
    /// Returns an error when `SQLite` cannot read the selected cached rows.
    pub fn widget_snapshot(
        &self,
        mailbox_ids: &[String],
        important_limit: u32,
    ) -> Result<WidgetSnapshot> {
        let mut selected_ids = mailbox_ids.to_vec();
        selected_ids.sort();
        selected_ids.dedup();
        selected_ids.truncate(MAX_WIDGET_MAILBOXES);

        let mut mailboxes = Vec::new();
        for mailbox_id in &selected_ids {
            let summary = self
                .connection
                .query_row(
                    "SELECT mb.id, mb.display_name, COUNT(mm.message_id)
                     FROM mailboxes AS mb
                     LEFT JOIN mailbox_messages AS mm
                       ON mm.mailbox_id = mb.id
                      AND mm.uid_validity = mb.uid_validity
                      AND mm.is_read = 0
                     WHERE mb.id = ?1
                     GROUP BY mb.id, mb.display_name",
                    [mailbox_id],
                    |row| {
                        Ok(WidgetMailboxSummary {
                            id: row.get(0)?,
                            display_name: row.get(1)?,
                            unread_count: u32_from_sql(row.get::<_, i64>(2)?, 2)?,
                        })
                    },
                )
                .optional()?;
            if let Some(summary) = summary {
                mailboxes.push(summary);
            }
        }

        let unread_count = mailboxes.iter().try_fold(0_u32, |total, mailbox| {
            total.checked_add(mailbox.unread_count).ok_or_else(|| {
                DatabaseError::Sqlite(rusqlite::Error::IntegralValueOutOfRange(2, i64::MAX))
            })
        })?;
        let important_limit = important_limit.min(MAX_WIDGET_MESSAGES);
        let important_messages = if selected_ids.is_empty() || important_limit == 0 {
            Vec::new()
        } else {
            let placeholders = std::iter::repeat_n("?", selected_ids.len())
                .collect::<Vec<_>>()
                .join(", ");
            let sql = format!(
                "SELECT msg.id, mm.mailbox_id, mm.remote_uid,
                        msg.subject, msg.sender, msg.date, msg.snippet,
                        mm.is_read, mm.is_starred, msg.has_attachments
                 FROM mailbox_messages AS mm
                 JOIN mailboxes AS mb ON mb.id = mm.mailbox_id
                 JOIN messages AS msg ON msg.id = mm.message_id
                 WHERE mm.mailbox_id IN ({placeholders})
                   AND mb.uid_validity IS NOT NULL
                   AND mm.uid_validity = mb.uid_validity
                   AND mm.is_starred = 1
                 ORDER BY msg.date DESC, mm.remote_uid DESC
                 LIMIT {important_limit}"
            );
            let mut statement = self.connection.prepare(&sql)?;
            let rows = statement.query_map(rusqlite::params_from_iter(&selected_ids), |row| {
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
            })?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };

        Ok(WidgetSnapshot {
            unread_count,
            mailboxes,
            important_messages,
        })
    }
}

// Schema text stays together so each migration transaction is reviewable.
#[allow(clippy::too_many_lines)]
fn migrate(connection: &Connection) -> Result<()> {
    let mut version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
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
                 has_attachments INTEGER NOT NULL DEFAULT 0,
                 thread_id TEXT,
                 canonical_message_id TEXT,
                 recipients_json TEXT NOT NULL DEFAULT '[]',
                 cc_json TEXT NOT NULL DEFAULT '[]',
                 sent_at INTEGER,
                 provider_message_id TEXT,
                 provider_thread_id TEXT,
                 content_json TEXT,
                 header_fingerprint TEXT NOT NULL DEFAULT ''
             );
             CREATE TABLE mailbox_messages (
                 mailbox_id TEXT NOT NULL REFERENCES mailboxes(id) ON DELETE CASCADE,
                 message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
                 uid_validity INTEGER NOT NULL,
                 remote_uid INTEGER NOT NULL,
                 is_read INTEGER NOT NULL DEFAULT 0,
                 is_starred INTEGER NOT NULL DEFAULT 0,
                 local_only INTEGER NOT NULL DEFAULT 0,
                 PRIMARY KEY(mailbox_id, uid_validity, remote_uid)
             );
             CREATE TABLE threads (
                 id TEXT PRIMARY KEY NOT NULL,
                 account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE
             );
             CREATE TABLE thread_keys (
                 account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
                 key TEXT NOT NULL,
                 thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
                 PRIMARY KEY(account_id, key)
             );
             CREATE TABLE pending_operations (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
                 mailbox_id TEXT NOT NULL,
                 remote_name TEXT NOT NULL,
                 message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
                 uid_validity INTEGER NOT NULL,
                 remote_uid INTEGER NOT NULL,
                 action TEXT NOT NULL,
                 destination_mailbox_id TEXT,
                 destination_remote_name TEXT,
                 state TEXT NOT NULL DEFAULT 'pending',
                 retry_count INTEGER NOT NULL DEFAULT 0,
                 last_error TEXT
             );
             CREATE INDEX mailbox_messages_page_idx
                 ON mailbox_messages(mailbox_id, uid_validity, remote_uid DESC);
             CREATE INDEX messages_account_idx ON messages(account_id);
             CREATE INDEX mailbox_messages_message_id_idx ON mailbox_messages(message_id);
             CREATE INDEX messages_thread_idx ON messages(thread_id, sent_at, date, id);
             CREATE INDEX messages_provider_idx ON messages(account_id, provider_message_id);
             CREATE INDEX pending_operations_account_idx
                 ON pending_operations(account_id, state, id);
             PRAGMA user_version = 3;
             COMMIT;",
        )?;
        version = 3;
    }
    let mut migration_open = false;
    if version == 1 {
        connection.execute_batch(
            "BEGIN IMMEDIATE;
             CREATE INDEX IF NOT EXISTS mailbox_messages_message_id_idx
                 ON mailbox_messages(message_id);",
        )?;
        version = 2;
        migration_open = true;
    }
    if version == 2 {
        if !migration_open {
            connection.execute_batch("BEGIN IMMEDIATE;")?;
        }
        connection.execute_batch(
            "ALTER TABLE messages ADD COLUMN thread_id TEXT;
             ALTER TABLE messages ADD COLUMN canonical_message_id TEXT;
             ALTER TABLE messages ADD COLUMN recipients_json TEXT NOT NULL DEFAULT '[]';
             ALTER TABLE messages ADD COLUMN cc_json TEXT NOT NULL DEFAULT '[]';
             ALTER TABLE messages ADD COLUMN sent_at INTEGER;
             ALTER TABLE messages ADD COLUMN provider_message_id TEXT;
             ALTER TABLE messages ADD COLUMN provider_thread_id TEXT;
             ALTER TABLE messages ADD COLUMN content_json TEXT;
             ALTER TABLE messages ADD COLUMN header_fingerprint TEXT NOT NULL DEFAULT '';
             ALTER TABLE mailbox_messages ADD COLUMN local_only INTEGER NOT NULL DEFAULT 0;
             CREATE TABLE threads (
                 id TEXT PRIMARY KEY NOT NULL,
                 account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE
             );
             CREATE TABLE thread_keys (
                 account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
                 key TEXT NOT NULL,
                 thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
                 PRIMARY KEY(account_id, key)
             );
             CREATE TABLE pending_operations (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
                 mailbox_id TEXT NOT NULL,
                 remote_name TEXT NOT NULL,
                 message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
                 uid_validity INTEGER NOT NULL,
                 remote_uid INTEGER NOT NULL,
                 action TEXT NOT NULL,
                 destination_mailbox_id TEXT,
                 destination_remote_name TEXT,
                 state TEXT NOT NULL DEFAULT 'pending',
                 retry_count INTEGER NOT NULL DEFAULT 0,
                 last_error TEXT
             );
             CREATE INDEX messages_thread_idx ON messages(thread_id, sent_at, date, id);
             CREATE INDEX messages_provider_idx ON messages(account_id, provider_message_id);
             CREATE INDEX pending_operations_account_idx
                 ON pending_operations(account_id, state, id);
             ",
        )?;
        backfill_threads(connection)?;
        connection.execute_batch("PRAGMA user_version = 3; COMMIT;")?;
        version = 3;
    }
    if version == 3 {
        migrate_search(connection)?;
        version = 4;
    }
    if version == 4 {
        migrate_message_details(connection)?;
    }
    Ok(())
}

fn migrate_search(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "BEGIN IMMEDIATE;
         UPDATE messages SET sent_at = unixepoch(date)
             WHERE sent_at IS NULL AND unixepoch(date) IS NOT NULL;
         CREATE INDEX messages_sent_at_idx ON messages(sent_at);
         CREATE INDEX messages_has_attachments_idx ON messages(has_attachments);
         CREATE INDEX mailbox_messages_unread_idx
             ON mailbox_messages(mailbox_id) WHERE is_read = 0;
         CREATE INDEX mailbox_messages_starred_idx
             ON mailbox_messages(mailbox_id) WHERE is_starred = 1;
         CREATE INDEX mailboxes_remote_name_idx ON mailboxes(remote_name COLLATE NOCASE);
         CREATE INDEX mailboxes_display_name_idx ON mailboxes(display_name COLLATE NOCASE);
         CREATE INDEX accounts_email_idx ON accounts(email COLLATE NOCASE);
         CREATE INDEX accounts_display_name_idx ON accounts(display_name COLLATE NOCASE);
         CREATE VIRTUAL TABLE messages_fts USING fts5(
             subject, sender, recipients_json, cc_json, snippet,
             content = 'messages', content_rowid = 'rowid'
         );
         CREATE TRIGGER messages_fts_insert AFTER INSERT ON messages BEGIN
             INSERT INTO messages_fts(
                 rowid, subject, sender, recipients_json, cc_json, snippet
             ) VALUES (
                 new.rowid, new.subject, new.sender, new.recipients_json, new.cc_json, new.snippet
             );
         END;
         CREATE TRIGGER messages_fts_delete AFTER DELETE ON messages BEGIN
             INSERT INTO messages_fts(
                 messages_fts, rowid, subject, sender, recipients_json, cc_json, snippet
             ) VALUES (
                 'delete', old.rowid, old.subject, old.sender,
                 old.recipients_json, old.cc_json, old.snippet
             );
         END;
         CREATE TRIGGER messages_fts_update AFTER UPDATE ON messages BEGIN
             INSERT INTO messages_fts(
                 messages_fts, rowid, subject, sender, recipients_json, cc_json, snippet
             ) VALUES (
                 'delete', old.rowid, old.subject, old.sender,
                 old.recipients_json, old.cc_json, old.snippet
             );
             INSERT INTO messages_fts(
                 rowid, subject, sender, recipients_json, cc_json, snippet
             ) VALUES (
                 new.rowid, new.subject, new.sender, new.recipients_json, new.cc_json, new.snippet
             );
         END;
         INSERT INTO messages_fts(messages_fts) VALUES ('rebuild');
         PRAGMA user_version = 4;
         COMMIT;",
    )?;
    Ok(())
}

fn migrate_message_details(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "BEGIN IMMEDIATE;
         ALTER TABLE messages ADD COLUMN senders_json TEXT NOT NULL DEFAULT '[]';
         ALTER TABLE messages ADD COLUMN bcc_json TEXT NOT NULL DEFAULT '[]';
         ALTER TABLE messages ADD COLUMN reply_to_json TEXT NOT NULL DEFAULT '[]';
         ALTER TABLE messages ADD COLUMN in_reply_to_json TEXT NOT NULL DEFAULT '[]';
         ALTER TABLE messages ADD COLUMN references_json TEXT NOT NULL DEFAULT '[]';
         ALTER TABLE messages ADD COLUMN list_id_json TEXT NOT NULL DEFAULT '[]';
         ALTER TABLE messages ADD COLUMN list_post_json TEXT NOT NULL DEFAULT '[]';
         ALTER TABLE messages ADD COLUMN list_unsubscribe_json TEXT NOT NULL DEFAULT '[]';
         ALTER TABLE messages ADD COLUMN authentication_results_json TEXT NOT NULL DEFAULT '[]';
         ALTER TABLE messages ADD COLUMN received_spf_json TEXT NOT NULL DEFAULT '[]';
         PRAGMA user_version = 5;
         COMMIT;",
    )?;
    Ok(())
}

fn backfill_threads(connection: &Connection) -> Result<()> {
    let mut statement = connection.prepare(
        "SELECT id, account_id, message_id_header, subject, sender
         FROM messages ORDER BY id",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    let mut canonical_counts = HashMap::new();
    for (_, account_id, header, _, _) in &rows {
        if let Some(canonical) = header.as_deref().and_then(canonical_message_id) {
            *canonical_counts
                .entry((account_id.clone(), canonical))
                .or_insert(0_u32) += 1;
        }
    }
    for (message_id, account_id, header, subject, sender) in rows {
        let thread_id = format!("thr:{}:{}", account_id.len(), message_id);
        let canonical = header.as_deref().and_then(canonical_message_id);
        let fingerprint = serde_json::to_string(&(
            &subject,
            &sender,
            Option::<i64>::None,
            Vec::<String>::new(),
            Vec::<String>::new(),
        ))?;
        connection.execute(
            "INSERT INTO threads (id, account_id) VALUES (?1, ?2)",
            params![thread_id, account_id],
        )?;
        connection.execute(
            "UPDATE messages
             SET thread_id = ?2, canonical_message_id = ?3, header_fingerprint = ?4
             WHERE id = ?1",
            params![message_id, thread_id, canonical, fingerprint],
        )?;
        if let Some(canonical) = canonical {
            if canonical_counts.get(&(account_id.clone(), canonical.clone())) == Some(&1) {
                connection.execute(
                    "INSERT INTO thread_keys (account_id, key, thread_id)
                     VALUES (?1, ?2, ?3)",
                    params![account_id, message_key(&canonical), thread_id],
                )?;
            }
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

fn invalidate_mailbox_memberships<T>(transaction: &T, mailbox_id: &str) -> Result<()>
where
    T: Deref<Target = Connection>,
{
    transaction.execute(
        "UPDATE pending_operations SET state = 'stale',
             last_error = 'mailbox UIDVALIDITY changed before replay'
         WHERE mailbox_id = ?1 AND state IN ('pending', 'failed')",
        [mailbox_id],
    )?;
    transaction.execute(
        "DELETE FROM mailbox_messages WHERE mailbox_id = ?1",
        [mailbox_id],
    )?;
    Ok(())
}

// Identity, content, thread, and membership writes are one atomic import unit.
#[allow(clippy::too_many_lines)]
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
    let canonical_message_id = header.message_id.as_deref().and_then(canonical_message_id);
    let recipients_json = serde_json::to_string(&header.recipients)?;
    let cc_json = serde_json::to_string(&header.cc)?;
    let content_json = header
        .content
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;
    let fingerprint = serde_json::to_string(&(
        &header.subject,
        &header.sender,
        header.sent_at,
        &header.recipients,
        &header.cc,
    ))?;
    let membership_message_id = transaction
        .query_row(
            "SELECT message_id FROM mailbox_messages
             WHERE mailbox_id = ?1 AND uid_validity = ?2 AND remote_uid = ?3",
            params![mailbox_id, i64::from(uid_validity), i64::from(header.uid)],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let provider_match = header
        .provider_message_id
        .as_deref()
        .map(|provider_id| {
            transaction
                .query_row(
                    "SELECT id FROM messages
                     WHERE account_id = ?1 AND provider_message_id = ?2
                     ORDER BY id LIMIT 1",
                    params![account_id, provider_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
        })
        .transpose()?
        .flatten();
    let projected_match = canonical_message_id
        .as_deref()
        .map(|canonical| {
            transaction
                .query_row(
                    "SELECT msg.id FROM mailbox_messages mm
                     JOIN messages msg ON msg.id = mm.message_id
                     WHERE mm.mailbox_id = ?1 AND mm.local_only = 1
                       AND msg.canonical_message_id = ?2
                       AND msg.header_fingerprint = ?3
                     ORDER BY msg.id LIMIT 1",
                    params![mailbox_id, canonical, fingerprint],
                    |row| row.get::<_, String>(0),
                )
                .optional()
        })
        .transpose()?
        .flatten();
    let queued_match = canonical_message_id
        .as_deref()
        .map(|canonical| {
            transaction
                .query_row(
                    "SELECT msg.id FROM pending_operations op
                     JOIN messages msg ON msg.id = op.message_id
                     WHERE op.mailbox_id = ?1 AND op.state = 'waiting'
                       AND msg.canonical_message_id = ?2
                       AND msg.header_fingerprint = ?3
                     ORDER BY op.id LIMIT 1",
                    params![mailbox_id, canonical, fingerprint],
                    |row| row.get::<_, String>(0),
                )
                .optional()
        })
        .transpose()?
        .flatten();
    let strong_metadata =
        !header.sender.is_empty() && !header.date.is_empty() && header.sent_at.is_some();
    let generic_match =
        if header.provider_message_id.is_none() && (content_json.is_some() || strong_metadata) {
            canonical_message_id
                .as_deref()
                .map(|canonical| {
                    transaction
                        .query_row(
                            "SELECT id FROM messages
                         WHERE account_id = ?1
                           AND canonical_message_id = ?2
                           AND header_fingerprint = ?3
                           AND EXISTS(
                               SELECT 1 FROM mailbox_messages mm
                               WHERE mm.message_id = messages.id AND mm.mailbox_id <> ?4
                           )
                           AND (content_json IS NULL OR ?5 IS NULL OR content_json = ?5)
                         ORDER BY id LIMIT 1",
                            params![account_id, canonical, fingerprint, mailbox_id, content_json],
                            |row| row.get::<_, String>(0),
                        )
                        .optional()
                })
                .transpose()?
                .flatten()
        } else {
            None
        };
    let message_row_id = membership_message_id
        .or(provider_match)
        .or(projected_match)
        .or(queued_match)
        .or(generic_match)
        .unwrap_or_else(|| message_id(mailbox_id, uid_validity, header.uid));
    let thread_id = resolve_thread(
        transaction,
        account_id,
        &message_row_id,
        canonical_message_id.as_deref(),
        &fingerprint,
        content_json.as_deref(),
        header,
    )?;
    transaction.execute(
        "INSERT INTO messages
            (id, account_id, message_id_header, subject, sender, date, snippet,
             has_attachments, thread_id, canonical_message_id, recipients_json,
             cc_json, sent_at, provider_message_id, provider_thread_id, content_json,
             header_fingerprint)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                 ?14, ?15, ?16, ?17)
         ON CONFLICT(id) DO UPDATE SET
            account_id = excluded.account_id,
            message_id_header = excluded.message_id_header,
            subject = excluded.subject,
            sender = excluded.sender,
            date = excluded.date,
            snippet = CASE WHEN excluded.content_json IS NULL
                           THEN messages.snippet ELSE excluded.snippet END,
            has_attachments = CASE WHEN excluded.content_json IS NULL
                                   THEN messages.has_attachments ELSE excluded.has_attachments END,
            thread_id = excluded.thread_id,
            canonical_message_id = excluded.canonical_message_id,
            recipients_json = excluded.recipients_json,
            cc_json = excluded.cc_json,
            sent_at = excluded.sent_at,
            provider_message_id = excluded.provider_message_id,
            provider_thread_id = excluded.provider_thread_id,
            content_json = COALESCE(excluded.content_json, messages.content_json),
            header_fingerprint = excluded.header_fingerprint",
        params![
            message_row_id,
            account_id,
            header.message_id,
            header.subject,
            header.sender,
            header.date,
            header
                .content
                .as_ref()
                .map(|content| content.plain_text.chars().take(240).collect::<String>())
                .unwrap_or_default(),
            i64::from(u8::from(
                header
                    .content
                    .as_ref()
                    .is_some_and(|content| !content.attachments.is_empty())
            )),
            thread_id,
            canonical_message_id,
            recipients_json,
            cc_json,
            header.sent_at,
            header.provider_message_id,
            header.provider_thread_id,
            content_json,
            fingerprint,
        ],
    )?;
    persist_message_details(transaction, &message_row_id, header)?;
    transaction.execute(
        "UPDATE pending_operations
         SET uid_validity = ?3, remote_uid = ?4, state = 'pending', last_error = NULL
         WHERE mailbox_id = ?1 AND message_id = ?2 AND state = 'waiting'",
        params![
            mailbox_id,
            message_row_id,
            i64::from(uid_validity),
            i64::from(header.uid)
        ],
    )?;
    let moved_away = transaction.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM pending_operations
             WHERE mailbox_id = ?1 AND message_id = ?2 AND uid_validity = ?3
               AND remote_uid = ?4 AND action = 'move'
               AND state IN ('pending', 'failed', 'waiting', 'completed')
         )",
        params![
            mailbox_id,
            message_row_id,
            i64::from(uid_validity),
            i64::from(header.uid)
        ],
        |row| Ok(row.get::<_, i64>(0)? != 0),
    )?;
    if moved_away {
        transaction.execute(
            "DELETE FROM mailbox_messages
             WHERE mailbox_id = ?1 AND uid_validity = ?2 AND remote_uid = ?3",
            params![mailbox_id, i64::from(uid_validity), i64::from(header.uid)],
        )?;
        return Ok(());
    }
    let projected_flags = transaction
        .query_row(
            "SELECT is_read, is_starred FROM mailbox_messages
             WHERE mailbox_id = ?1 AND message_id = ?2 AND local_only = 1",
            params![mailbox_id, message_row_id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()?;
    transaction.execute(
        "DELETE FROM mailbox_messages
         WHERE mailbox_id = ?1 AND message_id = ?2 AND local_only = 1",
        params![mailbox_id, message_row_id],
    )?;
    transaction.execute(
        "INSERT INTO mailbox_messages
            (mailbox_id, message_id, uid_validity, remote_uid, is_read, is_starred, local_only)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0)
         ON CONFLICT(mailbox_id, uid_validity, remote_uid) DO UPDATE SET
            message_id = excluded.message_id,
            is_read = excluded.is_read,
            is_starred = excluded.is_starred,
            local_only = 0",
        params![
            mailbox_id,
            message_row_id,
            i64::from(uid_validity),
            i64::from(header.uid),
            projected_flags.map_or_else(|| i64::from(u8::from(header.is_read)), |flags| flags.0),
            projected_flags.map_or_else(|| i64::from(u8::from(header.is_starred)), |flags| flags.1),
        ],
    )?;
    reapply_membership_overlays(
        transaction,
        mailbox_id,
        &message_row_id,
        uid_validity,
        header.uid,
    )?;
    Ok(())
}

fn persist_message_details<T>(
    transaction: &T,
    message_id: &str,
    header: &RemoteHeader,
) -> Result<()>
where
    T: Deref<Target = Connection>,
{
    transaction.execute(
        "UPDATE messages SET
             senders_json = ?2, bcc_json = ?3, reply_to_json = ?4,
             in_reply_to_json = ?5, references_json = ?6, list_id_json = ?7,
             list_post_json = ?8, list_unsubscribe_json = ?9,
             authentication_results_json = ?10, received_spf_json = ?11
         WHERE id = ?1",
        params![
            message_id,
            serde_json::to_string(&header.senders)?,
            serde_json::to_string(&header.bcc)?,
            serde_json::to_string(&header.reply_to)?,
            serde_json::to_string(&header.in_reply_to)?,
            serde_json::to_string(&header.references)?,
            serde_json::to_string(&header.list_id)?,
            serde_json::to_string(&header.list_post)?,
            serde_json::to_string(&header.list_unsubscribe)?,
            serde_json::to_string(&header.authentication_results)?,
            serde_json::to_string(&header.received_spf)?,
        ],
    )?;
    Ok(())
}

// Candidate discovery and component merging must remain one deterministic path.
#[allow(clippy::too_many_lines)]
fn resolve_thread<T>(
    transaction: &T,
    account_id: &str,
    message_row_id: &str,
    canonical_id: Option<&str>,
    fingerprint: &str,
    content_json: Option<&str>,
    header: &RemoteHeader,
) -> Result<String>
where
    T: Deref<Target = Connection>,
{
    let existing_thread = transaction
        .query_row(
            "SELECT thread_id FROM messages WHERE id = ?1",
            [message_row_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();

    let conflicting_duplicate = canonical_id
        .map(|id| {
            transaction.query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM messages
                     WHERE account_id = ?1 AND canonical_message_id = ?2 AND id <> ?3
                       AND (header_fingerprint <> ?4
                            OR (?5 IS NOT NULL AND content_json IS NOT NULL
                                AND content_json <> ?5))
                 )",
                params![account_id, id, message_row_id, fingerprint, content_json],
                |row| Ok(row.get::<_, i64>(0)? != 0),
            )
        })
        .transpose()?
        .unwrap_or(false);
    if conflicting_duplicate {
        if let Some(id) = canonical_id {
            transaction.execute(
                "DELETE FROM thread_keys WHERE account_id = ?1 AND key = ?2",
                params![account_id, message_key(id)],
            )?;
        }
    }

    let mut relation_ids = header
        .references
        .iter()
        .chain(&header.in_reply_to)
        .flat_map(|value| message_ids_in(value))
        .filter(|id| Some(id.as_str()) != canonical_id)
        .collect::<Vec<_>>();
    relation_ids.sort();
    relation_ids.dedup();
    relation_ids.retain(|id| !message_id_is_ambiguous(transaction, account_id, id).unwrap_or(true));
    if conflicting_duplicate {
        relation_ids.clear();
    }

    let provider_key = header
        .provider_thread_id
        .as_deref()
        .map(|id| format!("provider:{id}"));
    let own_key = canonical_id.map(message_key);
    let own_alias = own_key
        .as_deref()
        .map(|key| thread_for_key(transaction, account_id, key))
        .transpose()?
        .flatten();
    let mut candidates = Vec::new();
    if let Some(thread_id) = existing_thread {
        candidates.push(thread_id);
    }
    if let Some(key) = provider_key.as_deref() {
        if let Some(thread_id) = thread_for_key(transaction, account_id, key)? {
            candidates.push(thread_id);
        }
    }
    for relation in &relation_ids {
        if let Some(thread_id) = thread_for_key(transaction, account_id, &message_key(relation))? {
            candidates.push(thread_id);
        }
    }
    if let Some(alias) = own_alias {
        if !conflicting_duplicate {
            candidates.push(alias);
        }
    }
    candidates.sort();
    candidates.dedup();

    let thread_id = if candidates.is_empty() {
        let id = format!("thr:{}:{}", account_id.len(), message_row_id);
        transaction.execute(
            "INSERT OR IGNORE INTO threads (id, account_id) VALUES (?1, ?2)",
            params![id, account_id],
        )?;
        id
    } else {
        let winner = oldest_thread(transaction, &candidates)?;
        for loser in candidates.iter().filter(|candidate| *candidate != &winner) {
            transaction.execute(
                "UPDATE messages SET thread_id = ?1 WHERE thread_id = ?2",
                params![winner, loser],
            )?;
            transaction.execute(
                "UPDATE OR REPLACE thread_keys SET thread_id = ?1 WHERE thread_id = ?2",
                params![winner, loser],
            )?;
            transaction.execute("DELETE FROM threads WHERE id = ?1", [loser])?;
        }
        winner
    };

    if let Some(key) = provider_key {
        bind_thread_key(transaction, account_id, &key, &thread_id)?;
    }
    if !conflicting_duplicate {
        if let Some(key) = own_key {
            bind_thread_key(transaction, account_id, &key, &thread_id)?;
        }
    }
    for relation in relation_ids {
        bind_thread_key(transaction, account_id, &message_key(&relation), &thread_id)?;
    }
    Ok(thread_id)
}

fn oldest_thread<T>(transaction: &T, candidates: &[String]) -> Result<String>
where
    T: Deref<Target = Connection>,
{
    let mut winner = candidates[0].clone();
    let mut winner_rowid = i64::MAX;
    for candidate in candidates {
        let rowid = transaction.query_row(
            "SELECT rowid FROM threads WHERE id = ?1",
            [candidate],
            |row| row.get::<_, i64>(0),
        )?;
        if rowid < winner_rowid {
            winner.clone_from(candidate);
            winner_rowid = rowid;
        }
    }
    Ok(winner)
}

fn bind_thread_key<T>(transaction: &T, account_id: &str, key: &str, thread_id: &str) -> Result<()>
where
    T: Deref<Target = Connection>,
{
    transaction.execute(
        "INSERT INTO thread_keys (account_id, key, thread_id)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(account_id, key) DO NOTHING",
        params![account_id, key, thread_id],
    )?;
    Ok(())
}

fn thread_for_key<T>(transaction: &T, account_id: &str, key: &str) -> Result<Option<String>>
where
    T: Deref<Target = Connection>,
{
    transaction
        .query_row(
            "SELECT thread_id FROM thread_keys WHERE account_id = ?1 AND key = ?2",
            params![account_id, key],
            |row| row.get(0),
        )
        .optional()
        .map_err(DatabaseError::from)
}

fn message_id_is_ambiguous<T>(transaction: &T, account_id: &str, id: &str) -> Result<bool>
where
    T: Deref<Target = Connection>,
{
    transaction
        .query_row(
            "SELECT COUNT(DISTINCT header_fingerprint) > 1
             FROM messages WHERE account_id = ?1 AND canonical_message_id = ?2",
            params![account_id, id],
            |row| Ok(row.get::<_, i64>(0)? != 0),
        )
        .map_err(DatabaseError::from)
}

fn message_key(id: &str) -> String {
    format!("message:{id}")
}

fn message_ids_in(value: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let mut remainder = value;
    while let Some(start) = remainder.find('<') {
        let after_start = &remainder[start..];
        let Some(end) = after_start.find('>') else {
            break;
        };
        if let Some(id) = canonical_message_id(&after_start[..=end]) {
            ids.push(id);
        }
        remainder = &after_start[end + 1..];
    }
    if ids.is_empty() {
        if let Some(id) = canonical_message_id(value) {
            ids.push(id);
        }
    }
    ids
}

fn canonical_message_id(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let inner = if let Some(inner) = trimmed
        .strip_prefix('<')
        .and_then(|value| value.strip_suffix('>'))
    {
        inner
    } else if !trimmed.contains(['<', '>']) {
        trimmed
    } else {
        return None;
    };
    if inner.is_empty()
        || inner.chars().any(char::is_whitespace)
        || inner.contains(['<', '>'])
        || inner.matches('@').count() != 1
    {
        return None;
    }
    let (local, domain) = inner.split_once('@')?;
    let local = local
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(local);
    if local.is_empty() || domain.is_empty() {
        return None;
    }
    Some(format!("<{local}@{}>", domain.to_ascii_lowercase()))
}

fn reapply_membership_overlays<T>(
    transaction: &T,
    mailbox_id: &str,
    message_id: &str,
    uid_validity: u32,
    remote_uid: u32,
) -> Result<()>
where
    T: Deref<Target = Connection>,
{
    let mut statement = transaction.prepare(
        "SELECT action FROM pending_operations
         WHERE mailbox_id = ?1 AND message_id = ?2
           AND uid_validity = ?3 AND remote_uid = ?4
           AND state IN ('pending', 'failed')
         ORDER BY id",
    )?;
    let actions = statement
        .query_map(
            params![
                mailbox_id,
                message_id,
                i64::from(uid_validity),
                i64::from(remote_uid)
            ],
            |row| row.get::<_, String>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    for action in actions {
        match action.as_str() {
            "mark_read" => {
                transaction.execute(
                    "UPDATE mailbox_messages SET is_read = 1
                     WHERE mailbox_id = ?1 AND message_id = ?2",
                    params![mailbox_id, message_id],
                )?;
            }
            "mark_unread" => {
                transaction.execute(
                    "UPDATE mailbox_messages SET is_read = 0
                     WHERE mailbox_id = ?1 AND message_id = ?2",
                    params![mailbox_id, message_id],
                )?;
            }
            "star" => {
                transaction.execute(
                    "UPDATE mailbox_messages SET is_starred = 1
                     WHERE mailbox_id = ?1 AND message_id = ?2",
                    params![mailbox_id, message_id],
                )?;
            }
            "unstar" => {
                transaction.execute(
                    "UPDATE mailbox_messages SET is_starred = 0
                     WHERE mailbox_id = ?1 AND message_id = ?2",
                    params![mailbox_id, message_id],
                )?;
            }
            _ => {}
        }
    }
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
    use mail_model::{Attachment, MailAction, MessageContent};
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

    fn conversation_header(uid: u32, message_id: &str, references: &[&str]) -> RemoteHeader {
        RemoteHeader {
            uid,
            message_id: Some(message_id.into()),
            references: references.iter().map(|value| (*value).into()).collect(),
            subject: format!("Conversation {uid}"),
            sender: format!("sender{uid}@example.test"),
            date: format!("2026-01-01T00:{uid:02}:00Z"),
            sent_at: Some(i64::from(uid)),
            recipients: vec!["reader@example.test".into()],
            content: Some(MessageContent {
                plain_text: format!("body {uid}"),
                ..MessageContent::default()
            }),
            ..RemoteHeader::default()
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
    fn persists_complete_message_details_and_attachment_metadata() {
        let db = Database::open(":memory:").unwrap();
        db.upsert_account(&account("one")).unwrap();
        let mut snapshot = snapshot("INBOX", 1, &[4]);
        let header = &mut snapshot.headers[0];
        header.senders = vec!["First <first@example.test>".into(), "second@example.test".into()];
        header.recipients = vec!["To <to@example.test>".into()];
        header.reply_to = vec!["reply@example.test".into()];
        header.list_id = vec!["Tern <list.tern.example>".into()];
        header.authentication_results = vec!["mx.example; dkim=pass".into()];
        header.content = Some(MessageContent {
            attachments: vec![Attachment {
                id: "part-1".into(),
                filename: "report.pdf".into(),
                mime_type: "application/pdf".into(),
                data: vec![1, 2, 3, 4],
                content_id: None,
            }],
            ..MessageContent::default()
        });
        db.apply_snapshot("one", &snapshot).unwrap();

        let mailbox = db.list_mailboxes("one").unwrap().pop().unwrap();
        let message = db.list_messages(&mailbox.id, 0, 1).unwrap().pop().unwrap();
        let details = db.message_details(&message.id).unwrap().unwrap();
        assert_eq!(details.senders.len(), 2);
        assert_eq!(details.recipients, vec!["To <to@example.test>"]);
        assert_eq!(details.reply_to, vec!["reply@example.test"]);
        assert_eq!(details.authentication_results, vec!["mx.example; dkim=pass"]);
        assert_eq!(details.attachments[0].filename, "report.pdf");
        assert_eq!(details.attachments[0].size, 4);
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
    fn widget_snapshot_uses_only_selected_mailboxes_and_is_bounded() {
        let db = Database::open(":memory:").unwrap();
        db.upsert_account(&account("one")).unwrap();
        db.apply_snapshot("one", &snapshot("INBOX", 1, &[1, 2, 3, 6]))
            .unwrap();
        db.apply_snapshot("one", &snapshot("Archive", 1, &[9, 10, 12]))
            .unwrap();
        let mailboxes = db.list_mailboxes("one").unwrap();
        let inbox = mailboxes
            .iter()
            .find(|mailbox| mailbox.remote_name == "INBOX")
            .unwrap();

        let widget = db
            .widget_snapshot(&[inbox.id.clone(), inbox.id.clone(), "missing".into()], 1)
            .unwrap();

        assert_eq!(widget.unread_count, 2);
        assert_eq!(widget.mailboxes.len(), 1);
        assert_eq!(widget.mailboxes[0].display_name, "INBOX");
        assert_eq!(widget.mailboxes[0].unread_count, 2);
        assert_eq!(widget.important_messages.len(), 1);
        assert_eq!(widget.important_messages[0].remote_uid, 6);
    }

    #[test]
    fn empty_widget_selection_does_not_expose_cached_mail() {
        let db = Database::open(":memory:").unwrap();
        db.upsert_account(&account("one")).unwrap();
        db.apply_snapshot("one", &snapshot("INBOX", 1, &[1, 3]))
            .unwrap();

        let widget = db.widget_snapshot(&[], MAX_WIDGET_MESSAGES + 1).unwrap();

        assert_eq!(widget.unread_count, 0);
        assert!(widget.mailboxes.is_empty());
        assert!(widget.important_messages.is_empty());
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

    #[test]
    fn threads_normal_long_and_branched_replies_without_subject_grouping() {
        let db = Database::open(":memory:").unwrap();
        db.upsert_account(&account("one")).unwrap();
        let headers = vec![
            conversation_header(1, "root@example.test", &[]),
            conversation_header(2, "<two@example.test>", &["root@example.test"]),
            conversation_header(
                3,
                "<three@example.test>",
                &["<root@example.test>", "<two@example.test>"],
            ),
            conversation_header(4, "branch@example.test", &["<root@example.test>"]),
            conversation_header(5, "unrelated@example.test", &[]),
        ];
        let mailbox = db
            .apply_snapshot(
                "one",
                &MailboxSnapshot {
                    remote_name: "INBOX".into(),
                    uid_validity: 1,
                    uid_next: Some(6),
                    headers,
                },
            )
            .unwrap();
        let threads = db.list_threads(&mailbox.id, 0, 200).unwrap();
        assert_eq!(threads.len(), 2);
        let conversation = threads
            .iter()
            .find(|thread| thread.message_count == 4)
            .unwrap();
        let messages = db.list_thread_messages(&conversation.id, 0, 200).unwrap();
        assert_eq!(
            messages
                .iter()
                .map(|message| message.subject.as_str())
                .collect::<Vec<_>>(),
            vec![
                "Conversation 1",
                "Conversation 2",
                "Conversation 3",
                "Conversation 4"
            ]
        );
    }

    #[test]
    fn late_missing_parent_joins_phantom_thread_after_reopen() {
        let file = NamedTempFile::new().unwrap();
        let path = file.path().to_str().unwrap();
        let db = Database::open(path).unwrap();
        db.upsert_account(&account("one")).unwrap();
        let mailbox = db
            .apply_snapshot(
                "one",
                &MailboxSnapshot {
                    remote_name: "INBOX".into(),
                    uid_validity: 1,
                    uid_next: Some(3),
                    headers: vec![conversation_header(
                        2,
                        "child@example.test",
                        &["missing@example.test"],
                    )],
                },
            )
            .unwrap();
        drop(db);
        let db = Database::open(path).unwrap();
        db.apply_snapshot(
            "one",
            &MailboxSnapshot {
                remote_name: "INBOX".into(),
                uid_validity: 1,
                uid_next: Some(3),
                headers: vec![conversation_header(1, "missing@example.test", &[])],
            },
        )
        .unwrap();
        let threads = db.list_threads(&mailbox.id, 0, 20).unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].message_count, 2);
    }

    #[test]
    fn malformed_self_and_conflicting_duplicate_ids_do_not_bridge_threads() {
        let db = Database::open(":memory:").unwrap();
        db.upsert_account(&account("one")).unwrap();
        let mut duplicate = conversation_header(2, "dup@example.test", &["<bad"]);
        duplicate.sender = "attacker@example.test".into();
        duplicate.content.as_mut().unwrap().plain_text = "different".into();
        let mailbox = db
            .apply_snapshot(
                "one",
                &MailboxSnapshot {
                    remote_name: "INBOX".into(),
                    uid_validity: 1,
                    uid_next: Some(4),
                    headers: vec![
                        conversation_header(1, "dup@example.test", &["dup@example.test"]),
                        duplicate,
                        conversation_header(3, "other@example.test", &["missing-angle"]),
                    ],
                },
            )
            .unwrap();
        let threads = db.list_threads(&mailbox.id, 0, 20).unwrap();
        assert_eq!(threads.len(), 3);
    }

    #[test]
    fn cross_folder_generic_and_gmail_copies_deduplicate_with_local_flags() {
        let db = Database::open(":memory:").unwrap();
        db.upsert_account(&account("one")).unwrap();
        let generic = conversation_header(1, "copy@example.test", &[]);
        let inbox = db
            .apply_snapshot(
                "one",
                &MailboxSnapshot {
                    remote_name: "INBOX".into(),
                    uid_validity: 1,
                    uid_next: Some(2),
                    headers: vec![generic.clone()],
                },
            )
            .unwrap();
        let mut archive_copy = generic;
        archive_copy.uid = 9;
        archive_copy.is_read = true;
        let archive = db
            .apply_snapshot(
                "one",
                &MailboxSnapshot {
                    remote_name: "Archive".into(),
                    uid_validity: 7,
                    uid_next: Some(10),
                    headers: vec![archive_copy],
                },
            )
            .unwrap();
        let inbox_message = db.list_messages(&inbox.id, 0, 10).unwrap().remove(0);
        let archive_message = db.list_messages(&archive.id, 0, 10).unwrap().remove(0);
        assert_eq!(inbox_message.id, archive_message.id);
        let detail = db.get_thread_message(&inbox_message.id).unwrap().unwrap();
        assert_eq!(detail.memberships.len(), 2);
        assert!(!detail.memberships[0].is_read || !detail.memberships[1].is_read);

        let mut gmail_one = conversation_header(20, "g1@example.test", &[]);
        gmail_one.provider_message_id = Some("gm-1".into());
        gmail_one.provider_thread_id = Some("gt-1".into());
        let mut gmail_two = conversation_header(21, "g2@example.test", &[]);
        gmail_two.provider_message_id = Some("gm-2".into());
        gmail_two.provider_thread_id = Some("gt-1".into());
        db.apply_snapshot(
            "one",
            &MailboxSnapshot {
                remote_name: "INBOX".into(),
                uid_validity: 1,
                uid_next: Some(22),
                headers: vec![gmail_one, gmail_two],
            },
        )
        .unwrap();
        assert!(db
            .list_threads(&inbox.id, 0, 20)
            .unwrap()
            .iter()
            .any(|thread| thread.message_count == 2));
    }

    #[test]
    fn offline_flags_survive_refresh_and_uid_reset_stales_replay() {
        let db = Database::open(":memory:").unwrap();
        db.upsert_account(&account("one")).unwrap();
        let mut initial = snapshot("INBOX", 1, &[1]);
        initial.headers[0].is_read = false;
        let mailbox = db.apply_snapshot("one", &initial).unwrap();
        let message = db.list_messages(&mailbox.id, 0, 10).unwrap().remove(0);
        db.mutate_message(&message.id, &mailbox.id, MailAction::MarkRead, None)
            .unwrap();
        db.apply_snapshot("one", &initial).unwrap();
        assert!(db.list_messages(&mailbox.id, 0, 10).unwrap()[0].is_read);
        assert_eq!(db.pending_operations("one").unwrap().len(), 1);
        db.apply_snapshot("one", &snapshot("INBOX", 2, &[1]))
            .unwrap();
        let stale = db.pending_operations("one").unwrap();
        assert_eq!(stale.len(), 1);
        assert!(!stale[0].can_replay);
        assert!(stale[0]
            .last_error
            .as_deref()
            .unwrap()
            .contains("UIDVALIDITY"));
    }

    #[test]
    fn move_projects_locally_without_duplicate_and_reconciles_destination_uid() {
        let db = Database::open(":memory:").unwrap();
        db.upsert_account(&account("one")).unwrap();
        let original = conversation_header(1, "move@example.test", &[]);
        let inbox = db
            .apply_snapshot(
                "one",
                &MailboxSnapshot {
                    remote_name: "INBOX".into(),
                    uid_validity: 1,
                    uid_next: Some(2),
                    headers: vec![original.clone()],
                },
            )
            .unwrap();
        let archive = db
            .apply_snapshot("one", &snapshot("Archive", 4, &[]))
            .unwrap();
        let message = db.list_messages(&inbox.id, 0, 10).unwrap().remove(0);
        db.mutate_message(&message.id, &inbox.id, MailAction::Move, Some(&archive.id))
            .unwrap();
        assert!(db.list_messages(&inbox.id, 0, 10).unwrap().is_empty());
        assert_eq!(db.list_messages(&archive.id, 0, 10).unwrap().len(), 1);
        assert_eq!(
            db.search_messages("in:Archive", 0, 10).unwrap()[0].remote_uid,
            0
        );
        let mut confirmed = original;
        confirmed.uid = 40;
        db.apply_snapshot(
            "one",
            &MailboxSnapshot {
                remote_name: "Archive".into(),
                uid_validity: 4,
                uid_next: Some(41),
                headers: vec![confirmed],
            },
        )
        .unwrap();
        let messages = db.list_messages(&archive.id, 0, 10).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].remote_uid, 40);
    }

    #[test]
    fn failed_operations_persist_retry_details_across_reopen() {
        let file = NamedTempFile::new().unwrap();
        let path = file.path().to_str().unwrap();
        let db = Database::open(path).unwrap();
        db.upsert_account(&account("one")).unwrap();
        let mailbox = db
            .apply_snapshot("one", &snapshot("INBOX", 1, &[1]))
            .unwrap();
        let message = db.list_messages(&mailbox.id, 0, 10).unwrap().remove(0);
        db.mutate_message(&message.id, &mailbox.id, MailAction::Star, None)
            .unwrap();
        let operation = db.pending_operations("one").unwrap().remove(0);
        db.fail_operation(&operation.id, "temporary server failure")
            .unwrap();
        drop(db);

        let db = Database::open(path).unwrap();
        let operation = db.pending_operations("one").unwrap().remove(0);
        assert!(operation.can_replay);
        assert_eq!(operation.retry_count, 1);
        assert_eq!(
            operation.last_error.as_deref(),
            Some("temporary server failure")
        );
        db.complete_operation(&operation.id).unwrap();
        assert!(db.pending_operations("one").unwrap().is_empty());
    }

    #[test]
    fn body_cache_is_lazy_and_previous_plain_text_is_bounded() {
        let db = Database::open(":memory:").unwrap();
        db.upsert_account(&account("one")).unwrap();
        let headers = (1..=25)
            .map(|uid| {
                let references = (uid > 1).then_some("root@example.test");
                let message_id = if uid == 1 {
                    "root@example.test".to_owned()
                } else {
                    format!("{uid}@example.test")
                };
                conversation_header(uid, &message_id, references.as_slice())
            })
            .collect();
        let mailbox = db
            .apply_snapshot(
                "one",
                &MailboxSnapshot {
                    remote_name: "INBOX".into(),
                    uid_validity: 1,
                    uid_next: Some(26),
                    headers,
                },
            )
            .unwrap();
        assert_eq!(db.cached_body_uids(&mailbox.id).unwrap().len(), 25);
        let thread = db.list_threads(&mailbox.id, 0, 1).unwrap().remove(0);
        let metadata = db.list_thread_messages(&thread.id, 0, 200).unwrap();
        assert!(metadata.iter().all(|message| message.content.is_none()));
        let last = metadata.last().unwrap();
        assert!(db
            .get_thread_message(&last.id)
            .unwrap()
            .unwrap()
            .content
            .is_some());
        assert_eq!(db.previous_plain_texts(&last.id, 200).unwrap().len(), 20);
    }

    #[test]
    fn projected_actions_wait_then_bind_through_a_second_move() {
        let db = Database::open(":memory:").unwrap();
        db.upsert_account(&account("one")).unwrap();
        let original = conversation_header(1, "move-chain@example.test", &[]);
        let inbox = db
            .apply_snapshot(
                "one",
                &MailboxSnapshot {
                    remote_name: "INBOX".into(),
                    uid_validity: 1,
                    uid_next: Some(2),
                    headers: vec![original.clone()],
                },
            )
            .unwrap();
        let archive = db
            .apply_snapshot("one", &snapshot("Archive", 2, &[]))
            .unwrap();
        let trash = db
            .apply_snapshot("one", &snapshot("Trash", 3, &[]))
            .unwrap();
        let message = db.list_messages(&inbox.id, 0, 10).unwrap().remove(0);
        db.mutate_message(&message.id, &inbox.id, MailAction::Move, Some(&archive.id))
            .unwrap();
        db.mutate_message(&message.id, &archive.id, MailAction::MarkRead, None)
            .unwrap();
        db.mutate_message(&message.id, &archive.id, MailAction::Move, Some(&trash.id))
            .unwrap();
        let queued = db.pending_operations("one").unwrap();
        assert_eq!(
            queued
                .iter()
                .filter(|operation| operation.can_replay)
                .count(),
            1
        );
        assert_eq!(
            queued
                .iter()
                .filter(|operation| !operation.can_replay)
                .count(),
            2
        );
        assert!(db.list_messages(&archive.id, 0, 10).unwrap().is_empty());
        assert!(db.list_messages(&trash.id, 0, 10).unwrap()[0].is_read);

        let mut archive_confirmation = original.clone();
        archive_confirmation.uid = 20;
        db.apply_snapshot(
            "one",
            &MailboxSnapshot {
                remote_name: "Archive".into(),
                uid_validity: 2,
                uid_next: Some(21),
                headers: vec![archive_confirmation],
            },
        )
        .unwrap();
        let queued = db.pending_operations("one").unwrap();
        assert!(queued.iter().all(|operation| operation.can_replay));
        assert!(db.list_messages(&archive.id, 0, 10).unwrap().is_empty());

        let mut trash_confirmation = original;
        trash_confirmation.uid = 30;
        db.apply_snapshot(
            "one",
            &MailboxSnapshot {
                remote_name: "Trash".into(),
                uid_validity: 3,
                uid_next: Some(31),
                headers: vec![trash_confirmation],
            },
        )
        .unwrap();
        let messages = db.list_messages(&trash.id, 0, 10).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].remote_uid, 30);
        assert!(messages[0].is_read);
    }

    #[test]
    fn conflicting_bodies_with_same_metadata_do_not_share_a_thread() {
        let db = Database::open(":memory:").unwrap();
        db.upsert_account(&account("one")).unwrap();
        let first = conversation_header(1, "collision@example.test", &[]);
        let mut second = first.clone();
        second.uid = 2;
        second.content.as_mut().unwrap().plain_text = "different body".into();
        let mailbox = db
            .apply_snapshot(
                "one",
                &MailboxSnapshot {
                    remote_name: "INBOX".into(),
                    uid_validity: 1,
                    uid_next: Some(3),
                    headers: vec![first, second],
                },
            )
            .unwrap();
        assert_eq!(db.list_threads(&mailbox.id, 0, 20).unwrap().len(), 2);
    }

    #[test]
    fn cross_folder_body_enriches_header_only_logical_message() {
        let db = Database::open(":memory:").unwrap();
        db.upsert_account(&account("one")).unwrap();
        let full = conversation_header(1, "enrich@example.test", &[]);
        let mut header_only = full.clone();
        header_only.content = None;
        let inbox = db
            .apply_snapshot(
                "one",
                &MailboxSnapshot {
                    remote_name: "INBOX".into(),
                    uid_validity: 1,
                    uid_next: Some(2),
                    headers: vec![header_only],
                },
            )
            .unwrap();
        let mut sent_copy = full;
        sent_copy.uid = 9;
        let sent = db
            .apply_snapshot(
                "one",
                &MailboxSnapshot {
                    remote_name: "Sent".into(),
                    uid_validity: 2,
                    uid_next: Some(10),
                    headers: vec![sent_copy],
                },
            )
            .unwrap();
        let inbox_message = db.list_messages(&inbox.id, 0, 10).unwrap().remove(0);
        let sent_message = db.list_messages(&sent.id, 0, 10).unwrap().remove(0);
        assert_eq!(inbox_message.id, sent_message.id);
        let detail = db.get_thread_message(&inbox_message.id).unwrap().unwrap();
        assert_eq!(detail.memberships.len(), 2);
        assert_eq!(detail.content.unwrap().plain_text, "body 1");
    }

    #[test]
    fn conflicting_duplicate_body_threads_are_stable_on_repeat_snapshot() {
        let db = Database::open(":memory:").unwrap();
        db.upsert_account(&account("one")).unwrap();
        let first = conversation_header(1, "stable-collision@example.test", &[]);
        let mut second = first.clone();
        second.uid = 2;
        second.content.as_mut().unwrap().plain_text = "conflicting body".into();
        let snapshot = MailboxSnapshot {
            remote_name: "INBOX".into(),
            uid_validity: 1,
            uid_next: Some(3),
            headers: vec![first, second],
        };
        let mailbox = db.apply_snapshot("one", &snapshot).unwrap();
        let before = db
            .list_threads(&mailbox.id, 0, 20)
            .unwrap()
            .into_iter()
            .map(|thread| thread.id)
            .collect::<Vec<_>>();
        db.apply_snapshot("one", &snapshot).unwrap();
        let after = db
            .list_threads(&mailbox.id, 0, 20)
            .unwrap()
            .into_iter()
            .map(|thread| thread.id)
            .collect::<Vec<_>>();
        assert_eq!(before.len(), 2);
        assert_eq!(after, before);
    }

    #[test]
    fn long_conversation_pages_across_bounded_snapshots() {
        let db = Database::open(":memory:").unwrap();
        db.upsert_account(&account("one")).unwrap();
        let make_headers = |range: std::ops::RangeInclusive<u32>| {
            range
                .map(|uid| {
                    let id = format!("long-{uid}@example.test");
                    let parent = (uid > 1).then(|| format!("long-{}@example.test", uid - 1));
                    let references = parent.as_deref().into_iter().collect::<Vec<_>>();
                    conversation_header(uid, &id, &references)
                })
                .collect()
        };
        let mailbox = db
            .apply_snapshot(
                "one",
                &MailboxSnapshot {
                    remote_name: "INBOX".into(),
                    uid_validity: 1,
                    uid_next: Some(201),
                    headers: make_headers(1..=200),
                },
            )
            .unwrap();
        db.apply_snapshot(
            "one",
            &MailboxSnapshot {
                remote_name: "INBOX".into(),
                uid_validity: 1,
                uid_next: Some(251),
                headers: make_headers(201..=250),
            },
        )
        .unwrap();
        let thread = db.list_threads(&mailbox.id, 0, 10).unwrap().remove(0);
        assert_eq!(thread.message_count, 250);
        assert_eq!(
            db.list_thread_messages(&thread.id, 0, 10_000)
                .unwrap()
                .len(),
            200
        );
        assert_eq!(
            db.list_thread_messages(&thread.id, 200, 200).unwrap().len(),
            50
        );
    }

    #[test]
    fn provider_identity_is_account_scoped_and_deduplicates_labels() {
        let db = Database::open(":memory:").unwrap();
        db.upsert_account(&account("one")).unwrap();
        db.upsert_account(&account("two")).unwrap();
        let mut provider = conversation_header(1, "provider@example.test", &[]);
        provider.provider_message_id = Some("provider-message".into());
        provider.provider_thread_id = Some("provider-thread".into());
        let one_inbox = db
            .apply_snapshot(
                "one",
                &MailboxSnapshot {
                    remote_name: "INBOX".into(),
                    uid_validity: 1,
                    uid_next: Some(2),
                    headers: vec![provider.clone()],
                },
            )
            .unwrap();
        let mut label_copy = provider.clone();
        label_copy.uid = 7;
        let one_label = db
            .apply_snapshot(
                "one",
                &MailboxSnapshot {
                    remote_name: "Label".into(),
                    uid_validity: 2,
                    uid_next: Some(8),
                    headers: vec![label_copy],
                },
            )
            .unwrap();
        let two_inbox = db
            .apply_snapshot(
                "two",
                &MailboxSnapshot {
                    remote_name: "INBOX".into(),
                    uid_validity: 1,
                    uid_next: Some(2),
                    headers: vec![provider],
                },
            )
            .unwrap();
        let one = db.list_messages(&one_inbox.id, 0, 10).unwrap().remove(0);
        let label = db.list_messages(&one_label.id, 0, 10).unwrap().remove(0);
        let two = db.list_messages(&two_inbox.id, 0, 10).unwrap().remove(0);
        assert_eq!(one.id, label.id);
        assert_ne!(one.id, two.id);
        assert_ne!(
            db.list_threads(&one_inbox.id, 0, 10).unwrap()[0].id,
            db.list_threads(&two_inbox.id, 0, 10).unwrap()[0].id
        );
    }

    #[test]
    fn same_subject_stays_separate_and_header_refresh_keeps_body() {
        let db = Database::open(":memory:").unwrap();
        db.upsert_account(&account("one")).unwrap();
        let first = conversation_header(1, "first@example.test", &[]);
        let mut second = conversation_header(2, "second@example.test", &[]);
        second.subject.clone_from(&first.subject);
        let mailbox = db
            .apply_snapshot(
                "one",
                &MailboxSnapshot {
                    remote_name: "INBOX".into(),
                    uid_validity: 1,
                    uid_next: Some(3),
                    headers: vec![first.clone(), second],
                },
            )
            .unwrap();
        assert_eq!(db.list_threads(&mailbox.id, 0, 20).unwrap().len(), 2);
        let message = db.list_messages(&mailbox.id, 0, 20).unwrap().remove(1);
        let mut header_only = first;
        header_only.content = None;
        db.apply_snapshot(
            "one",
            &MailboxSnapshot {
                remote_name: "INBOX".into(),
                uid_validity: 1,
                uid_next: Some(3),
                headers: vec![header_only],
            },
        )
        .unwrap();
        assert!(db
            .get_thread_message(&message.id)
            .unwrap()
            .unwrap()
            .content
            .is_some());
    }

    #[test]
    fn version_two_migration_preserves_cache_and_links_new_reply() {
        let file = NamedTempFile::new().unwrap();
        let path = file.path().to_str().unwrap();
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch(
                "PRAGMA foreign_keys = ON;
                 CREATE TABLE accounts (
                     id TEXT PRIMARY KEY NOT NULL, email TEXT NOT NULL,
                     display_name TEXT NOT NULL, imap_host TEXT NOT NULL,
                     imap_port INTEGER NOT NULL, username TEXT NOT NULL,
                     credential_ref TEXT NOT NULL
                 );
                 CREATE TABLE mailboxes (
                     id TEXT PRIMARY KEY NOT NULL,
                     account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
                     remote_name TEXT NOT NULL, display_name TEXT NOT NULL,
                     uid_validity INTEGER, uid_next INTEGER,
                     UNIQUE(account_id, remote_name)
                 );
                 CREATE TABLE messages (
                     id TEXT PRIMARY KEY NOT NULL,
                     account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
                     message_id_header TEXT, subject TEXT NOT NULL, sender TEXT NOT NULL,
                     date TEXT NOT NULL, snippet TEXT NOT NULL DEFAULT '',
                     has_attachments INTEGER NOT NULL DEFAULT 0
                 );
                 CREATE TABLE mailbox_messages (
                     mailbox_id TEXT NOT NULL REFERENCES mailboxes(id) ON DELETE CASCADE,
                     message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
                     uid_validity INTEGER NOT NULL, remote_uid INTEGER NOT NULL,
                     is_read INTEGER NOT NULL DEFAULT 0,
                     is_starred INTEGER NOT NULL DEFAULT 0,
                     PRIMARY KEY(mailbox_id, uid_validity, remote_uid)
                 );
                 CREATE INDEX mailbox_messages_message_id_idx
                     ON mailbox_messages(message_id);
                 INSERT INTO accounts VALUES
                     ('one', 'one@example.test', 'One', 'imap.example.test', 993,
                      'one', 'keychain://one');
                 INSERT INTO mailboxes VALUES
                     ('mbx:3:one:5:INBOX', 'one', 'INBOX', 'INBOX', 1, 2);
                 INSERT INTO messages VALUES
                     ('legacy-parent', 'one', 'parent@example.test', 'Parent',
                      'sender@example.test', '2026-01-01T00:00:00Z', '', 0);
                 INSERT INTO mailbox_messages VALUES
                     ('mbx:3:one:5:INBOX', 'legacy-parent', 1, 1, 0, 0);
                 PRAGMA user_version = 2;",
            )
            .unwrap();
        drop(connection);

        let db = Database::open(path).unwrap();
        db.apply_snapshot(
            "one",
            &MailboxSnapshot {
                remote_name: "INBOX".into(),
                uid_validity: 1,
                uid_next: Some(3),
                headers: vec![conversation_header(
                    2,
                    "reply@example.test",
                    &["parent@example.test"],
                )],
            },
        )
        .unwrap();
        let threads = db.list_threads("mbx:3:one:5:INBOX", 0, 20).unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].message_count, 2);
        let search = db
            .search_messages("subject:Parent before:2026-02-01", 0, 20)
            .unwrap();
        assert_eq!(search.len(), 1);
        assert_eq!(search[0].id, "legacy-parent");
    }

    #[test]
    fn structured_search_combines_fts_and_relational_filters() {
        let db = Database::open(":memory:").unwrap();
        db.upsert_account(&account("work")).unwrap();
        db.upsert_account(&account("personal")).unwrap();

        let matching = RemoteHeader {
            uid: 1,
            message_id: Some("<roadmap@example.test>".into()),
            subject: "Quarterly roadmap".into(),
            sender: "Ada Lovelace <ada@example.test>".into(),
            date: "2026-09-15T12:00:00Z".into(),
            recipients: vec!["Tern Team <team@example.test>".into()],
            sent_at: Some(1_789_473_600),
            content: Some(MessageContent {
                plain_text: "The launch plan is ready.".into(),
                attachments: vec![Attachment {
                    filename: "plan.pdf".into(),
                    ..Attachment::default()
                }],
                ..MessageContent::default()
            }),
            is_read: false,
            is_starred: true,
            ..RemoteHeader::default()
        };
        let decoy = RemoteHeader {
            uid: 2,
            message_id: Some("<budget@example.test>".into()),
            subject: "Quarterly budget".into(),
            sender: "Grace Hopper <grace@example.test>".into(),
            date: "2026-10-15T12:00:00Z".into(),
            recipients: vec!["Other <other@example.test>".into()],
            sent_at: Some(1_792_065_600),
            content: Some(MessageContent {
                plain_text: "The launch plan changed.".into(),
                ..MessageContent::default()
            }),
            is_read: true,
            is_starred: false,
            ..RemoteHeader::default()
        };
        db.apply_snapshot(
            "work",
            &MailboxSnapshot {
                remote_name: "INBOX".into(),
                uid_validity: 1,
                uid_next: Some(3),
                headers: vec![matching.clone(), decoy],
            },
        )
        .unwrap();
        let mut archived = matching.clone();
        archived.uid = 3;
        db.apply_snapshot(
            "work",
            &MailboxSnapshot {
                remote_name: "Archive".into(),
                uid_validity: 1,
                uid_next: Some(4),
                headers: vec![archived],
            },
        )
        .unwrap();
        let mut personal = matching;
        personal.uid = 4;
        db.apply_snapshot(
            "personal",
            &MailboxSnapshot {
                remote_name: "INBOX".into(),
                uid_validity: 1,
                uid_next: Some(5),
                headers: vec![personal],
            },
        )
        .unwrap();

        let results = db
            .search_messages(
                "launch from:ada@example.test to:team@example.test subject:roadmap \
                 after:2026-09-01 before:2026-10-01 has:attachment is:unread \
                 is:starred in:INBOX account:work",
                0,
                20,
            )
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].remote_uid, 1);
        assert_eq!(results[0].snippet, "The launch plan is ready.");
        assert!(results[0].has_attachments);

        assert_eq!(
            db.search_messages("subject:budget", 0, 20).unwrap().len(),
            1
        );
        assert_eq!(db.search_messages("in:Archive", 0, 20).unwrap().len(), 1);
        assert_eq!(
            db.search_messages("account:personal", 0, 20).unwrap().len(),
            1
        );
        assert_eq!(db.search_messages("roadmap", 0, 20).unwrap().len(), 2);
    }

    #[test]
    fn reimport_updates_the_fts_index() {
        let db = Database::open(":memory:").unwrap();
        db.upsert_account(&account("one")).unwrap();
        db.apply_snapshot("one", &snapshot("INBOX", 1, &[1]))
            .unwrap();
        assert_eq!(db.search_messages("Subject", 0, 20).unwrap().len(), 1);

        let mut changed = snapshot("INBOX", 1, &[1]);
        changed.headers[0].subject = "Renamed".into();
        db.apply_snapshot("one", &changed).unwrap();

        assert!(db.search_messages("Subject", 0, 20).unwrap().is_empty());
        assert_eq!(db.search_messages("Renamed", 0, 20).unwrap().len(), 1);
    }
}
