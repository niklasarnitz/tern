use mail_model::{MessageContent, MessageMembership, ThreadMessage, ThreadSummary};
use rusqlite::{params, OptionalExtension};

use crate::{Database, DatabaseError, Result, MAX_PAGE_SIZE};

impl Database {
    /// Lists bounded conversation summaries represented in one mailbox.
    ///
    /// # Errors
    /// Returns an error when `SQLite` cannot read the requested page.
    pub fn list_threads(
        &self,
        mailbox_id: &str,
        offset: u32,
        limit: u32,
    ) -> Result<Vec<ThreadSummary>> {
        let limit = limit.min(MAX_PAGE_SIZE);
        let mut statement = self.connection.prepare(
            "SELECT msg.thread_id
             FROM mailbox_messages mm
             JOIN messages msg ON msg.id = mm.message_id
             WHERE mm.mailbox_id = ?1 AND msg.thread_id IS NOT NULL
             GROUP BY msg.thread_id
             ORDER BY MAX(COALESCE(msg.sent_at, unixepoch(msg.date), 0)) DESC,
                      MAX(msg.date) DESC, msg.thread_id
             LIMIT ?2 OFFSET ?3",
        )?;
        let thread_ids = statement
            .query_map(
                params![mailbox_id, i64::from(limit), i64::from(offset)],
                |row| row.get::<_, String>(0),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        thread_ids
            .into_iter()
            .map(|thread_id| self.thread_summary(mailbox_id, &thread_id))
            .collect()
    }

    /// Lists bounded message metadata in chronological order across account folders.
    ///
    /// # Errors
    /// Returns an error when `SQLite` cannot read the requested page.
    pub fn list_thread_messages(
        &self,
        thread_id: &str,
        offset: u32,
        limit: u32,
    ) -> Result<Vec<ThreadMessage>> {
        let limit = limit.min(MAX_PAGE_SIZE);
        let mut statement = self.connection.prepare(
            "SELECT id FROM messages WHERE thread_id = ?1
             ORDER BY COALESCE(sent_at, unixepoch(date), 0), date, id
             LIMIT ?2 OFFSET ?3",
        )?;
        let ids = statement
            .query_map(
                params![thread_id, i64::from(limit), i64::from(offset)],
                |row| row.get::<_, String>(0),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        ids.into_iter()
            .map(|id| self.thread_message(&id, false))
            .collect()
    }

    /// Reads one message with its persisted body and attachments.
    ///
    /// # Errors
    /// Returns an error when `SQLite` cannot read or decode the message.
    pub fn get_thread_message(&self, message_id: &str) -> Result<Option<ThreadMessage>> {
        let exists = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM messages WHERE id = ?1)",
            [message_id],
            |row| Ok(row.get::<_, i64>(0)? != 0),
        )?;
        exists
            .then(|| self.thread_message(message_id, true))
            .transpose()
    }

    /// Returns UIDs whose normalized body is already cached in this mailbox.
    ///
    /// # Errors
    /// Returns an error when `SQLite` cannot read the mailbox.
    pub fn cached_body_uids(&self, mailbox_id: &str) -> Result<Vec<u32>> {
        let mut statement = self.connection.prepare(
            "SELECT mm.remote_uid
             FROM mailbox_messages mm
             JOIN messages msg ON msg.id = mm.message_id
             JOIN mailboxes mb ON mb.id = mm.mailbox_id
             WHERE mm.mailbox_id = ?1 AND mm.uid_validity = mb.uid_validity
               AND mm.local_only = 0 AND msg.content_json IS NOT NULL
             ORDER BY mm.remote_uid",
        )?;
        let values = statement
            .query_map([mailbox_id], |row| row.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        values
            .into_iter()
            .map(|value| crate::u32_from_sql(value, 0).map_err(DatabaseError::from))
            .collect()
    }

    /// Returns up to twenty earlier plain-text bodies from the same conversation.
    ///
    /// # Errors
    /// Returns an error when `SQLite` cannot read the conversation.
    pub fn previous_plain_texts(&self, message_id: &str, limit: u32) -> Result<Vec<String>> {
        let limit = limit.min(20);
        let target = self
            .connection
            .query_row(
                "SELECT thread_id, COALESCE(sent_at, unixepoch(date), 0), date, id
                 FROM messages WHERE id = ?1",
                [message_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((thread_id, order_time, date, id)) = target else {
            return Ok(Vec::new());
        };
        let mut statement = self.connection.prepare(
            "SELECT json_extract(content_json, '$.plain_text')
             FROM messages
             WHERE thread_id = ?1 AND content_json IS NOT NULL
               AND (COALESCE(sent_at, unixepoch(date), 0) < ?2
                    OR (COALESCE(sent_at, unixepoch(date), 0) = ?2 AND date < ?3)
                    OR (COALESCE(sent_at, unixepoch(date), 0) = ?2 AND date = ?3 AND id < ?4))
             ORDER BY COALESCE(sent_at, unixepoch(date), 0) DESC, date DESC, id DESC
             LIMIT ?5",
        )?;
        let mut texts = statement
            .query_map(
                params![thread_id, order_time, date, id, i64::from(limit)],
                |row| row.get::<_, String>(0),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        texts.reverse();
        Ok(texts)
    }

    fn thread_summary(&self, mailbox_id: &str, thread_id: &str) -> Result<ThreadSummary> {
        let (id, subject, sender, date, snippet) = self.connection.query_row(
            "SELECT msg.thread_id, msg.subject, msg.sender, msg.date, msg.snippet
                 FROM mailbox_messages mm
                 JOIN messages msg ON msg.id = mm.message_id
                 WHERE mm.mailbox_id = ?1 AND msg.thread_id = ?2
                 ORDER BY COALESCE(msg.sent_at, unixepoch(msg.date), 0) DESC,
                          msg.date DESC, msg.id DESC
                 LIMIT 1",
            params![mailbox_id, thread_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )?;
        let (is_starred, has_attachments) = self.connection.query_row(
            "SELECT MAX(mm.is_starred), MAX(msg.has_attachments)
             FROM mailbox_messages mm
             JOIN messages msg ON msg.id = mm.message_id
             WHERE mm.mailbox_id = ?1 AND msg.thread_id = ?2",
            params![mailbox_id, thread_id],
            |row| Ok((row.get::<_, i64>(0)? != 0, row.get::<_, i64>(1)? != 0)),
        )?;
        let message_count = self.connection.query_row(
            "SELECT COUNT(*) FROM messages WHERE thread_id = ?1",
            [thread_id],
            |row| crate::u32_from_sql(row.get::<_, i64>(0)?, 0),
        )?;
        let unread_count = self.connection.query_row(
            "SELECT COUNT(*) FROM mailbox_messages mm
             JOIN messages msg ON msg.id = mm.message_id
             WHERE mm.mailbox_id = ?1 AND msg.thread_id = ?2 AND mm.is_read = 0",
            params![mailbox_id, thread_id],
            |row| crate::u32_from_sql(row.get::<_, i64>(0)?, 0),
        )?;
        Ok(ThreadSummary {
            id,
            mailbox_id: mailbox_id.to_owned(),
            subject,
            sender,
            date,
            snippet,
            message_count,
            unread_count,
            is_starred,
            has_attachments,
        })
    }

    fn thread_message(&self, message_id: &str, include_content: bool) -> Result<ThreadMessage> {
        let row = self.connection.query_row(
            "SELECT id, thread_id, subject, sender, date, recipients_json, cc_json,
                    CASE WHEN ?2 THEN content_json ELSE NULL END
             FROM messages WHERE id = ?1",
            params![message_id, include_content],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ))
            },
        )?;
        let mut membership = self.connection.prepare(
            "SELECT mailbox_id, is_read, is_starred FROM mailbox_messages
             WHERE message_id = ?1 ORDER BY mailbox_id",
        )?;
        let memberships = membership
            .query_map([message_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)? != 0,
                    row.get::<_, i64>(2)? != 0,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let is_read = memberships.iter().all(|(_, read, _)| *read);
        let is_starred = memberships.iter().any(|(_, _, starred)| *starred);
        let mailbox_ids = memberships
            .iter()
            .map(|(mailbox_id, _, _)| mailbox_id.clone())
            .collect();
        let membership_records = memberships
            .iter()
            .map(|(mailbox_id, is_read, is_starred)| MessageMembership {
                mailbox_id: mailbox_id.clone(),
                is_read: *is_read,
                is_starred: *is_starred,
            })
            .collect();
        Ok(ThreadMessage {
            id: row.0,
            thread_id: row.1,
            subject: row.2,
            sender: row.3,
            date: row.4,
            recipients: serde_json::from_str(&row.5)?,
            cc: serde_json::from_str(&row.6)?,
            mailbox_ids,
            memberships: membership_records,
            is_read,
            is_starred,
            content: row
                .7
                .as_deref()
                .map(serde_json::from_str::<MessageContent>)
                .transpose()?,
            display_plain_text: None,
        })
    }
}
