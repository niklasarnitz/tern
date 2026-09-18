use mail_model::{MailAction, PendingOperation};
use rusqlite::{params, OptionalExtension, Transaction};

use crate::{Database, DatabaseError, Result};

impl Database {
    /// Applies one optimistic mailbox-scoped mutation and queues remote replay atomically.
    ///
    /// # Errors
    /// Returns an error if the membership is absent or the transaction fails.
    pub fn mutate_message(
        &self,
        message_id: &str,
        mailbox_id: &str,
        action: MailAction,
        destination_mailbox_id: Option<&str>,
    ) -> Result<()> {
        let transaction = self.connection.unchecked_transaction()?;
        mutate_one(
            &transaction,
            message_id,
            mailbox_id,
            action,
            destination_mailbox_id,
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Applies a mutation only to this thread's memberships in the selected mailbox.
    ///
    /// # Errors
    /// Returns an error if the thread has no selected membership or the transaction fails.
    pub fn mutate_thread(
        &self,
        thread_id: &str,
        mailbox_id: &str,
        action: MailAction,
        destination_mailbox_id: Option<&str>,
    ) -> Result<()> {
        let transaction = self.connection.unchecked_transaction()?;
        let mut statement = transaction.prepare(
            "SELECT DISTINCT mm.message_id
             FROM mailbox_messages mm
             JOIN messages msg ON msg.id = mm.message_id
             WHERE mm.mailbox_id = ?1 AND msg.thread_id = ?2",
        )?;
        let ids = statement
            .query_map(params![mailbox_id, thread_id], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        if ids.is_empty() {
            return Err(DatabaseError::ThreadNotInMailbox {
                thread_id: thread_id.to_owned(),
                mailbox_id: mailbox_id.to_owned(),
            });
        }
        for id in ids {
            mutate_one(
                &transaction,
                &id,
                mailbox_id,
                action,
                destination_mailbox_id,
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Returns replayable operations, including failed attempts, in enqueue order.
    ///
    /// # Errors
    /// Returns an error when `SQLite` cannot read the queue.
    pub fn pending_operations(&self, account_id: &str) -> Result<Vec<PendingOperation>> {
        let mut statement = self.connection.prepare(
            "SELECT id, account_id, mailbox_id, remote_name, message_id, uid_validity,
                    remote_uid, action, destination_mailbox_id, destination_remote_name,
                    retry_count, last_error, state
             FROM pending_operations
             WHERE account_id = ?1 AND state <> 'completed'
             ORDER BY id",
        )?;
        let rows = statement.query_map([account_id], |row| {
            let action = action_from_db(&row.get::<_, String>(7)?)?;
            Ok(PendingOperation {
                id: row.get::<_, i64>(0)?.to_string(),
                account_id: row.get(1)?,
                mailbox_id: row.get(2)?,
                remote_name: row.get(3)?,
                message_id: row.get(4)?,
                uid_validity: crate::u32_from_sql(row.get::<_, i64>(5)?, 5)?,
                remote_uid: crate::u32_from_sql(row.get::<_, i64>(6)?, 6)?,
                action,
                destination_mailbox_id: row.get(8)?,
                destination_remote_name: row.get(9)?,
                retry_count: crate::u32_from_sql(row.get::<_, i64>(10)?, 10)?,
                last_error: row.get(11)?,
                can_replay: matches!(row.get::<_, String>(12)?.as_str(), "pending" | "failed"),
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(DatabaseError::from)
    }

    /// Marks a replay as acknowledged while retaining reconciliation evidence.
    ///
    /// # Errors
    /// Returns an error when the operation is absent or `SQLite` rejects the update.
    pub fn complete_operation(&self, id: &str) -> Result<()> {
        let changed = self.connection.execute(
            "UPDATE pending_operations SET state = 'completed', last_error = NULL WHERE id = ?1",
            [id],
        )?;
        if changed == 0 {
            return Err(DatabaseError::PendingOperationNotFound(id.to_owned()));
        }
        Ok(())
    }

    /// Records a replay failure durably and leaves the operation retryable.
    ///
    /// # Errors
    /// Returns an error when the operation is absent or `SQLite` rejects the update.
    pub fn fail_operation(&self, id: &str, reason: &str) -> Result<()> {
        let changed = self.connection.execute(
            "UPDATE pending_operations
             SET state = 'failed', retry_count = retry_count + 1, last_error = ?2
             WHERE id = ?1",
            params![id, reason],
        )?;
        if changed == 0 {
            return Err(DatabaseError::PendingOperationNotFound(id.to_owned()));
        }
        Ok(())
    }
}

// Keeping queue creation and its local projection together makes the atomicity
// of this state transition auditable.
#[allow(clippy::too_many_lines)]
fn mutate_one(
    transaction: &Transaction<'_>,
    message_id: &str,
    mailbox_id: &str,
    action: MailAction,
    destination_mailbox_id: Option<&str>,
) -> Result<()> {
    let membership = transaction
        .query_row(
            "SELECT mb.account_id, mb.remote_name, mm.uid_validity, mm.remote_uid,
                    mm.local_only
             FROM mailbox_messages mm
             JOIN mailboxes mb ON mb.id = mm.mailbox_id
             WHERE mm.message_id = ?1 AND mm.mailbox_id = ?2 AND mm.local_only = 0
             ORDER BY mm.remote_uid LIMIT 1",
            params![message_id, mailbox_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)? != 0,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| DatabaseError::MessageNotInMailbox {
            message_id: message_id.to_owned(),
            mailbox_id: mailbox_id.to_owned(),
        })?;
    let destination = if action == MailAction::Move {
        let id = destination_mailbox_id.ok_or(DatabaseError::InvalidMoveDestination)?;
        Some(
            transaction
                .query_row(
                    "SELECT id, remote_name, uid_validity FROM mailboxes
                 WHERE id = ?1 AND account_id = ?2",
                    params![id, membership.0],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, Option<i64>>(2)?,
                        ))
                    },
                )
                .optional()?
                .ok_or(DatabaseError::InvalidMoveDestination)?,
        )
    } else {
        None
    };
    let action_name = action_to_db(action);
    transaction.execute(
        "INSERT INTO pending_operations
            (account_id, mailbox_id, remote_name, message_id, uid_validity, remote_uid,
             action, destination_mailbox_id, destination_remote_name, state)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            membership.0,
            mailbox_id,
            membership.1,
            message_id,
            membership.2,
            if membership.4 { 0 } else { membership.3 },
            action_name,
            destination.as_ref().map(|value| &value.0),
            destination.as_ref().map(|value| &value.1),
            if membership.4 { "waiting" } else { "pending" },
        ],
    )?;
    match action {
        MailAction::MarkRead => update_flag(transaction, mailbox_id, message_id, "is_read", true)?,
        MailAction::MarkUnread => {
            update_flag(transaction, mailbox_id, message_id, "is_read", false)?;
        }
        MailAction::Star => update_flag(transaction, mailbox_id, message_id, "is_starred", true)?,
        MailAction::Unstar => {
            update_flag(transaction, mailbox_id, message_id, "is_starred", false)?;
        }
        MailAction::Move => {
            let (destination_id, _, destination_validity) =
                destination.ok_or(DatabaseError::InvalidMoveDestination)?;
            let validity = destination_validity.ok_or(DatabaseError::InvalidMoveDestination)?;
            let flags = transaction.query_row(
                "SELECT is_read, is_starred FROM mailbox_messages
                 WHERE mailbox_id = ?1 AND message_id = ?2",
                params![mailbox_id, message_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )?;
            transaction.execute(
                "DELETE FROM mailbox_messages WHERE mailbox_id = ?1 AND message_id = ?2",
                params![mailbox_id, message_id],
            )?;
            let destination_has_message = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM mailbox_messages
                               WHERE mailbox_id = ?1 AND message_id = ?2)",
                params![destination_id, message_id],
                |row| Ok(row.get::<_, i64>(0)? != 0),
            )?;
            if !destination_has_message {
                let projected_uid = -transaction.last_insert_rowid();
                transaction.execute(
                    "INSERT INTO mailbox_messages
                    (mailbox_id, message_id, uid_validity, remote_uid,
                     is_read, is_starred, local_only)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1)",
                    params![
                        destination_id,
                        message_id,
                        validity,
                        projected_uid,
                        flags.0,
                        flags.1
                    ],
                )?;
            }
        }
    }
    Ok(())
}

fn update_flag(
    transaction: &Transaction<'_>,
    mailbox_id: &str,
    message_id: &str,
    column: &str,
    value: bool,
) -> Result<()> {
    let sql = match column {
        "is_read" => {
            "UPDATE mailbox_messages SET is_read = ?3 WHERE mailbox_id = ?1 AND message_id = ?2"
        }
        _ => {
            "UPDATE mailbox_messages SET is_starred = ?3 WHERE mailbox_id = ?1 AND message_id = ?2"
        }
    };
    transaction.execute(
        sql,
        params![mailbox_id, message_id, i64::from(u8::from(value))],
    )?;
    Ok(())
}

fn action_to_db(action: MailAction) -> &'static str {
    match action {
        MailAction::MarkRead => "mark_read",
        MailAction::MarkUnread => "mark_unread",
        MailAction::Star => "star",
        MailAction::Unstar => "unstar",
        MailAction::Move => "move",
    }
}

fn action_from_db(value: &str) -> rusqlite::Result<MailAction> {
    match value {
        "mark_read" => Ok(MailAction::MarkRead),
        "mark_unread" => Ok(MailAction::MarkUnread),
        "star" => Ok(MailAction::Star),
        "unstar" => Ok(MailAction::Unstar),
        "move" => Ok(MailAction::Move),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}
