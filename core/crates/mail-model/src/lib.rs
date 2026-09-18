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
#[derive(Clone, Debug)]
pub struct RemoteHeader {
    pub uid: u32,
    pub message_id: Option<String>,
    pub subject: String,
    pub sender: String,
    pub date: String,
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
