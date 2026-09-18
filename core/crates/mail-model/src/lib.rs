//! Portable application records. Secrets are never persisted in these records.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, uniffi::Record)]
pub struct Account {
    pub id: String,
    pub email: String,
    pub display_name: String,
    pub imap_host: String,
    pub imap_port: u16,
    pub username: String,
    pub credential_ref: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, uniffi::Record)]
pub struct Mailbox {
    pub id: String,
    pub account_id: String,
    pub remote_name: String,
    pub display_name: String,
    pub uid_validity: Option<u32>,
    pub uid_next: Option<u32>,
}
#[derive(Clone, Debug, Serialize, Deserialize, uniffi::Record)]
pub struct MessageSummary {
    pub id: String,
    pub mailbox_id: String,
    pub remote_uid: u32,
    pub subject: String,
    pub sender: String,
    pub date: String,
    pub snippet: String,
    pub is_read: bool,
    pub is_starred: bool,
    pub has_attachments: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct Attachment {
    pub id: String,
    pub filename: String,
    pub mime_type: String,
    pub content_id: Option<String>,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct MessageContent {
    pub plain_text: String,
    pub html: String,
    pub attachments: Vec<Attachment>,
    pub delivery_report: Option<DeliveryReport>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct DeliveryReport {
    pub reporting_mta: String,
    pub recipients: Vec<DeliveryRecipient>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct DeliveryRecipient {
    pub recipient: String,
    pub action: String,
    pub status_code: String,
    pub status_description: String,
    pub diagnostic: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, uniffi::Record)]
pub struct ThreadSummary {
    pub id: String,
    pub mailbox_id: String,
    pub subject: String,
    pub sender: String,
    pub date: String,
    pub snippet: String,
    pub message_count: u32,
    pub unread_count: u32,
    pub is_starred: bool,
    pub has_attachments: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, uniffi::Record)]
pub struct ThreadMessage {
    pub id: String,
    pub thread_id: String,
    pub subject: String,
    pub sender: String,
    pub date: String,
    pub recipients: Vec<String>,
    pub cc: Vec<String>,
    pub mailbox_ids: Vec<String>,
    pub memberships: Vec<MessageMembership>,
    pub is_read: bool,
    pub is_starred: bool,
    pub content: Option<MessageContent>,
    pub display_plain_text: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, uniffi::Record)]
pub struct MessageMembership {
    pub mailbox_id: String,
    pub is_read: bool,
    pub is_starred: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
pub enum MailAction {
    MarkRead,
    MarkUnread,
    Star,
    Unstar,
    Move,
}

#[derive(Clone, Debug, Serialize, Deserialize, uniffi::Record)]
pub struct PendingOperation {
    pub id: String,
    pub account_id: String,
    pub mailbox_id: String,
    pub remote_name: String,
    pub message_id: String,
    pub uid_validity: u32,
    pub remote_uid: u32,
    pub action: MailAction,
    pub destination_mailbox_id: Option<String>,
    pub destination_remote_name: Option<String>,
    pub retry_count: u32,
    pub last_error: Option<String>,
    pub can_replay: bool,
}

#[derive(Clone, Debug, Default)]
pub struct RemoteHeader {
    pub uid: u32,
    pub message_id: Option<String>,
    pub in_reply_to: Vec<String>,
    pub references: Vec<String>,
    pub subject: String,
    pub sender: String,
    pub date: String,
    pub recipients: Vec<String>,
    pub cc: Vec<String>,
    pub sent_at: Option<i64>,
    pub provider_message_id: Option<String>,
    pub provider_thread_id: Option<String>,
    pub content: Option<MessageContent>,
    pub is_read: bool,
    pub is_starred: bool,
}
#[derive(Clone, Debug)]
pub struct MailboxSnapshot {
    pub remote_name: String,
    pub uid_validity: u32,
    pub uid_next: Option<u32>,
    pub headers: Vec<RemoteHeader>,
}

uniffi::setup_scaffolding!();
