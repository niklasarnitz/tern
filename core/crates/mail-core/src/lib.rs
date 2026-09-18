//! Stable application boundary exposed to native frontends via `UniFFI`.
use mail_db::Database;
use mail_model::{
    Account, AccountDiscovery, DiscoveryOverrides, Mailbox, MessageSummary, WidgetSnapshot,
};
use std::sync::{Arc, Mutex};

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum MailError {
    #[error("Local mail storage: {message}")]
    Storage { message: String },
    #[error("Account discovery: {message}")]
    Discovery { message: String },
}

/// Discover an implicit-TLS IMAP configuration without authenticating.
///
/// Manual values take precedence over provider presets, domain autoconfig,
/// DNS SRV records, and conservative hostname guesses.
///
/// # Errors
/// Returns a sanitized error when the email address or override is invalid or
/// the platform cannot initialize discovery.
#[uniffi::export]
pub async fn discover_account(
    email: String,
    overrides: DiscoveryOverrides,
) -> Result<AccountDiscovery, MailError> {
    mail_autoconfig::discover(&email, &overrides)
        .await
        .map_err(|error| MailError::Discovery {
            message: error.to_string(),
        })
}
fn storage(error: impl std::fmt::Display) -> MailError {
    MailError::Storage {
        message: error.to_string(),
    }
}

#[derive(uniffi::Object)]
pub struct MailClient {
    database: Mutex<Database>,
}
#[uniffi::export]
#[allow(
    clippy::needless_pass_by_value,
    reason = "UniFFI requires owned strings at the foreign API boundary"
)]
impl MailClient {
    /// Open the canonical local store.
    ///
    /// # Errors
    /// Returns a storage error if the database cannot be opened or migrated.
    #[uniffi::constructor]
    pub fn new(database_path: String) -> Result<Arc<Self>, MailError> {
        Ok(Arc::new(Self {
            database: Mutex::new(Database::open(&database_path).map_err(storage)?),
        }))
    }
    /// List configured accounts without network access.
    ///
    /// # Errors
    /// Returns a storage error if the database cannot be read.
    pub fn list_accounts(&self) -> Result<Vec<Account>, MailError> {
        self.database
            .lock()
            .map_err(storage)?
            .list_accounts()
            .map_err(storage)
    }
    /// List cached mailboxes for one account.
    ///
    /// # Errors
    /// Returns a storage error if the database cannot be read.
    pub fn list_mailboxes(&self, account_id: String) -> Result<Vec<Mailbox>, MailError> {
        self.database
            .lock()
            .map_err(storage)?
            .list_mailboxes(&account_id)
            .map_err(storage)
    }
    /// Read a bounded page from the local cache.
    ///
    /// # Errors
    /// Returns a storage error if the database cannot be read.
    pub fn list_messages(
        &self,
        mailbox_id: String,
        offset: u32,
        limit: u32,
    ) -> Result<Vec<MessageSummary>, MailError> {
        self.database
            .lock()
            .map_err(storage)?
            .list_messages(&mailbox_id, offset, limit)
            .map_err(storage)
    }
    /// Search the local cache with free text and structured operators.
    ///
    /// # Errors
    /// Returns a storage error if the query is malformed or the database
    /// cannot be read.
    pub fn search_messages(
        &self,
        query: String,
        offset: u32,
        limit: u32,
    ) -> Result<Vec<MessageSummary>, MailError> {
        self.database
            .lock()
            .map_err(storage)?
            .search_messages(&query, offset, limit)
            .map_err(storage)
    }

    /// Read bounded widget data for explicitly selected mailboxes.
    ///
    /// # Errors
    /// Returns a storage error if the database cannot be read.
    pub fn widget_snapshot(
        &self,
        mailbox_ids: Vec<String>,
        important_limit: u32,
    ) -> Result<WidgetSnapshot, MailError> {
        self.database
            .lock()
            .map_err(storage)?
            .widget_snapshot(&mailbox_ids, important_limit)
            .map_err(storage)
    }

    /// Optimistically move a cached message and keep it undoable until the deadline.
    ///
    /// The operation remains local until a later authenticated synchronization
    /// claims it after `undo_deadline_ms`.
    ///
    /// # Errors
    /// Returns a storage error if the source or destination is unavailable.
    pub fn queue_message_move(
        &self,
        mailbox_id: String,
        message_id: String,
        destination_mailbox_id: String,
        undo_deadline_ms: i64,
    ) -> Result<String, MailError> {
        self.database
            .lock()
            .map_err(storage)?
            .queue_move(
                &mailbox_id,
                &message_id,
                &destination_mailbox_id,
                undo_deadline_ms,
            )
            .map_err(storage)
    }

    /// Cancel a local operation before synchronization claims it.
    ///
    /// # Errors
    /// Returns a storage error if the operation queue cannot be updated.
    pub fn undo_operation(&self, operation_id: String) -> Result<bool, MailError> {
        self.database
            .lock()
            .map_err(storage)?
            .undo_operation(&operation_id)
            .map_err(storage)
    }
}
uniffi::setup_scaffolding!();
